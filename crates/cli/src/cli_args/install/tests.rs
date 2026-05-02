use super::InstallDependencyOptions;
use pacquet_package_manifest::DependencyGroup;
use pretty_assertions::assert_eq;

#[test]
fn dependency_options_to_dependency_groups() {
    use DependencyGroup::{Dev, Optional, Prod};
    let create_list = |opts: InstallDependencyOptions| opts.dependency_groups().collect::<Vec<_>>();

    // no flags -> prod + dev + optional
    assert_eq!(
        create_list(InstallDependencyOptions { prod: false, dev: false, no_optional: false }),
        [Prod, Dev, Optional],
    );

    // --prod -> prod + optional
    assert_eq!(
        create_list(InstallDependencyOptions { prod: true, dev: false, no_optional: false }),
        [Prod, Optional],
    );

    // --dev -> dev + optional
    assert_eq!(
        create_list(InstallDependencyOptions { prod: false, dev: true, no_optional: false }),
        [Dev, Optional],
    );

    // --no-optional -> prod + dev
    assert_eq!(
        create_list(InstallDependencyOptions { prod: false, dev: false, no_optional: true }),
        [Prod, Dev],
    );

    // --prod --no-optional -> prod
    assert_eq!(
        create_list(InstallDependencyOptions { prod: true, dev: false, no_optional: true }),
        [Prod],
    );

    // --dev --no-optional -> dev
    assert_eq!(
        create_list(InstallDependencyOptions { prod: false, dev: true, no_optional: true }),
        [Dev],
    );

    // --prod --dev -> prod + dev + optional
    assert_eq!(
        create_list(InstallDependencyOptions { prod: true, dev: true, no_optional: false }),
        [Prod, Dev, Optional],
    );

    // --prod --dev --no-optional -> prod + dev
    assert_eq!(
        create_list(InstallDependencyOptions { prod: true, dev: true, no_optional: true }),
        [Prod, Dev],
    );
}
