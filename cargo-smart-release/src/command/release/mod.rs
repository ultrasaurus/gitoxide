use crate::command::release::Options;
use anyhow::bail;
use cargo_metadata::{camino::Utf8PathBuf, Dependency, DependencyKind, Metadata, Package};
use git_repository::{refs::packed, Repository};
use std::collections::BTreeSet;

mod utils;
use git_repository::hash::ObjectId;
use utils::{
    bump_spec_may_cause_empty_commits, bump_version, is_dependency_with_version_requirement, is_workspace_member,
    names_and_versions, package_by_id, package_by_name, package_eq_dependency, package_for_dependency, tag_name_for,
    will, workspace_package_by_id,
};

mod cargo;
mod git;
mod manifest;

pub(in crate::command::release_impl) struct Context {
    root: Utf8PathBuf,
    meta: Metadata,
    repo: Repository,
    packed_refs: Option<packed::Buffer>,
}

impl Context {
    fn new() -> anyhow::Result<Self> {
        let meta = cargo_metadata::MetadataCommand::new().exec()?;
        let root = meta.workspace_root.clone();
        let repo = git_repository::discover(&root)?;
        let packed_refs = repo.refs.packed()?;
        Ok(Context {
            root,
            repo,
            meta,
            packed_refs,
        })
    }
}

/// In order to try dealing with https://github.com/sunng87/cargo-release/issues/224 and also to make workspace
/// releases more selective.
pub fn release(options: Options, version_bump_spec: String, mut crates: Vec<String>) -> anyhow::Result<()> {
    if options.no_dry_run_cargo_publish && !options.dry_run {
        bail!("The --no-dry-run-cargo-publish flag is only effective without --execute")
    }
    let ctx = Context::new()?;
    if crates.is_empty() {
        let crate_name = std::env::current_dir()?
            .file_name()
            .expect("a valid directory with a name")
            .to_str()
            .expect("directory is UTF8 representable")
            .to_owned();
        log::warn!(
            "Using '{}' as crate name as no one was provided. Specify one if this isn't correct",
            crate_name
        );
        crates = vec![crate_name];
    }
    release_depth_first(ctx, crates, &version_bump_spec, options)?;
    Ok(())
}

fn release_depth_first(
    ctx: Context,
    crate_names: Vec<String>,
    bump_spec: &str,
    options: Options,
) -> anyhow::Result<()> {
    let meta = &ctx.meta;
    let changed_crate_names_to_publish =
        traverse_dependencies_and_find_crates_for_publishing(meta, &crate_names, &ctx, options)?;

    let crates_to_publish_together = resolve_cycles_with_publish_group(meta, &changed_crate_names_to_publish, options)?;

    for publishee_name in changed_crate_names_to_publish
        .iter()
        .filter(|n| !crates_to_publish_together.contains(n))
    {
        let publishee = package_by_name(meta, publishee_name)?;

        let (new_version, commit_id) = perform_single_release(meta, publishee, options, bump_spec, &ctx)?;
        git::create_version_tag(publishee, &new_version, commit_id, &ctx.repo, options)?;
    }

    if !crates_to_publish_together.is_empty() {
        perforrm_multi_version_release(&ctx, bump_spec, options, meta, crates_to_publish_together)?;
    }

    Ok(())
}

fn perforrm_multi_version_release(
    ctx: &Context,
    bump_spec: &str,
    options: Options,
    meta: &Metadata,
    crates_to_publish_together: Vec<String>,
) -> anyhow::Result<()> {
    let mut crates_to_publish_together = crates_to_publish_together
        .into_iter()
        .map(|name| {
            let p = package_by_name(meta, &name)?;
            bump_version(&p.version.to_string(), bump_spec).map(|v| (p, v.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    log::info!(
        "{} prepare releases of {}",
        will(options.dry_run),
        names_and_versions(&crates_to_publish_together)
    );

    let commit_id = manifest::edit_version_and_fixup_dependent_crates(
        meta,
        &crates_to_publish_together,
        bump_spec_may_cause_empty_commits(bump_spec),
        options,
        &ctx,
    )?;

    crates_to_publish_together.reverse();
    while let Some((publishee, new_version)) = crates_to_publish_together.pop() {
        let unpublished_crates: Vec<_> = crates_to_publish_together
            .iter()
            .map(|(p, _)| p.name.to_owned())
            .collect();
        cargo::publish_crate(publishee, &unpublished_crates, options)?;
        git::create_version_tag(publishee, &new_version, commit_id, &ctx.repo, options)?;
    }
    Ok(())
}

fn resolve_cycles_with_publish_group(
    meta: &Metadata,
    changed_crate_names_to_publish: &[String],
    options: Options,
) -> anyhow::Result<Vec<String>> {
    let mut crates_to_publish_additionally_to_avoid_instability = Vec::new();
    let mut publish_group = Vec::<String>::new();
    for publishee_name in changed_crate_names_to_publish.iter() {
        let publishee = package_by_name(meta, publishee_name)?;
        let cycles = workspace_members_referring_to_publishee(meta, publishee);
        if cycles.is_empty() {
            log::debug!("'{}' is cycle-free", publishee.name);
        } else {
            for Cycle { from, hops } in cycles {
                log::warn!(
                    "'{}' links to '{}' {} causing publishes to never settle.",
                    publishee.name,
                    from.name,
                    if hops == 1 {
                        "directly".to_string()
                    } else {
                        format!("via {} hops", hops)
                    }
                );
                if !changed_crate_names_to_publish.contains(&from.name) {
                    crates_to_publish_additionally_to_avoid_instability.push(from.name.clone());
                } else {
                    for name in &[&from.name, &publishee.name] {
                        if !publish_group.contains(name) {
                            publish_group.push(name.to_string())
                        }
                    }
                }
            }
        }
    }
    if !crates_to_publish_additionally_to_avoid_instability.is_empty() && !options.ignore_instability {
        bail!(
            "Refusing to publish unless --ignore-instability is provided or crate(s) {} is/are included in the publish. To avoid this, don't specify versions in your dev dependencies.",
            crates_to_publish_additionally_to_avoid_instability.join(", ")
        )
    }
    Ok(reorder_according_to_existing_order(
        changed_crate_names_to_publish,
        &publish_group,
    ))
}

fn traverse_dependencies_and_find_crates_for_publishing(
    meta: &Metadata,
    crate_names: &[String],
    ctx: &Context,
    Options {
        allow_auto_publish_of_stable_crates,
        ..
    }: Options,
) -> anyhow::Result<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut changed_crate_names_to_publish = Vec::new();
    for crate_name in crate_names {
        if seen.contains(crate_name) {
            continue;
        }
        if dependency_tree_has_link_to_existing_crate_names(meta, crate_name, &changed_crate_names_to_publish)? {
            // redo all work which includes the previous tree. Could be more efficient but that would be more complicated.
            seen.clear();
            changed_crate_names_to_publish.clear();
        }
        let num_crates_for_publishing_without_dependencies = changed_crate_names_to_publish.len();
        let package = package_by_name(meta, crate_name)?;
        depth_first_traversal(
            meta,
            ctx,
            allow_auto_publish_of_stable_crates,
            &mut seen,
            &mut changed_crate_names_to_publish,
            package,
        )?;
        if num_crates_for_publishing_without_dependencies == changed_crate_names_to_publish.len() {
            let crate_package = package_by_name(meta, crate_name)?;
            if !git::has_changed_since_last_release(crate_package, ctx)? {
                log::info!(
                    "Skipping provided {} v{} hasn't changed since last released",
                    crate_package.name,
                    crate_package.version
                );
            }
            continue;
        }
        changed_crate_names_to_publish.push(crate_name.to_owned());
        seen.insert(crate_name.to_owned());
    }
    Ok(changed_crate_names_to_publish)
}

fn depth_first_traversal(
    meta: &Metadata,
    ctx: &Context,
    allow_auto_publish_of_stable_crates: bool,
    seen: &mut BTreeSet<String>,
    changed_crate_names_to_publish: &mut Vec<String>,
    package: &Package,
) -> anyhow::Result<()> {
    for dependency in package.dependencies.iter().filter(|d| d.kind == DependencyKind::Normal) {
        if seen.contains(&dependency.name) || !is_workspace_member(meta, &dependency.name) {
            continue;
        }
        seen.insert(dependency.name.clone());
        let dep_package = package_by_name(meta, &dependency.name)?;
        depth_first_traversal(
            meta,
            ctx,
            allow_auto_publish_of_stable_crates,
            seen,
            changed_crate_names_to_publish,
            dep_package,
        )?;
        if git::has_changed_since_last_release(dep_package, ctx)? {
            if dep_package.version.major == 0 || allow_auto_publish_of_stable_crates {
                log::info!(
                    "Adding {} v{} to set of published crates as it changed since last release",
                    dep_package.name,
                    dep_package.version
                );
                changed_crate_names_to_publish.push(dependency.name.clone());
            } else {
                log::warn!(
                    "{} v{} changed since last release - consider releasing it beforehand.",
                    dep_package.name,
                    dep_package.version
                );
            }
        } else {
            log::info!(
                "{} v{}  - skipped release as it didn't change",
                dep_package.name,
                dep_package.version
            );
        }
    }
    Ok(())
}

fn dependency_tree_has_link_to_existing_crate_names(
    meta: &Metadata,
    root_name: &str,
    existing_names: &[String],
) -> anyhow::Result<bool> {
    let mut dependency_names = vec![root_name];
    let mut seen = BTreeSet::new();
    while let Some(crate_name) = dependency_names.pop() {
        if !seen.insert(crate_name) {
            continue;
        }
        if existing_names.iter().any(|n| n == crate_name) {
            return Ok(true);
        }
        dependency_names.extend(
            package_by_name(meta, crate_name)?
                .dependencies
                .iter()
                .filter(|dep| is_workspace_member(meta, &dep.name))
                .map(|dep| dep.name.as_str()),
        )
    }
    Ok(false)
}

fn reorder_according_to_existing_order(reference_order: &[String], names_to_order: &[String]) -> Vec<String> {
    let new_order = reference_order
        .iter()
        .filter(|name| names_to_order.contains(name))
        .fold(Vec::new(), |mut acc, name| {
            acc.push(name.clone());
            acc
        });
    assert_eq!(
        new_order.len(),
        names_to_order.len(),
        "the reference order must contain all items to be ordered"
    );
    new_order
}

struct Cycle<'a> {
    from: &'a Package,
    hops: usize,
}

fn workspace_members_referring_to_publishee<'a>(meta: &'a Metadata, publishee: &Package) -> Vec<Cycle<'a>> {
    publishee
        .dependencies
        .iter()
        .filter(|dep| is_dependency_with_version_requirement(dep)) // unspecified versions don't matter for publishing
        .filter(|dep| {
            dep.kind != DependencyKind::Normal
                && meta
                    .workspace_members
                    .iter()
                    .map(|id| package_by_id(meta, id))
                    .any(|potential_cycle| package_eq_dependency(potential_cycle, dep))
        })
        .filter_map(|dep| {
            hops_for_dependency_to_link_back_to_publishee(meta, dep, publishee).map(|hops| Cycle {
                hops,
                from: package_by_name(meta, &dep.name).expect("package exists"),
            })
        })
        .collect()
}

fn hops_for_dependency_to_link_back_to_publishee<'a>(
    meta: &'a Metadata,
    source: &Dependency,
    destination: &Package,
) -> Option<usize> {
    let source = package_for_dependency(meta, source);
    let mut package_ids = vec![(0, &source.id)];
    let mut seen = BTreeSet::new();
    while let Some((level, id)) = package_ids.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(package) = workspace_package_by_id(meta, id) {
            if package
                .dependencies
                .iter()
                .any(|dep| package_eq_dependency(destination, dep))
            {
                return Some(level + 1);
            }
            package_ids.extend(
                package
                    .dependencies
                    .iter()
                    .map(|dep| (level + 1, &package_for_dependency(meta, dep).id)),
            );
        };
    }
    None
}

fn perform_single_release(
    meta: &Metadata,
    publishee: &Package,
    options: Options,
    bump_spec: &str,
    ctx: &Context,
) -> anyhow::Result<(String, ObjectId)> {
    let new_version = bump_version(&publishee.version.to_string(), bump_spec)?.to_string();
    log::info!(
        "{} prepare release of {} v{}",
        will(options.dry_run),
        publishee.name,
        new_version
    );
    let commit_id = manifest::edit_version_and_fixup_dependent_crates(
        meta,
        &[(publishee, new_version.clone())],
        bump_spec_may_cause_empty_commits(bump_spec),
        options,
        ctx,
    )?;
    cargo::publish_crate(publishee, &[], options)?;
    Ok((new_version, commit_id))
}