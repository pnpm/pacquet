use std::path::{ MAIN_SEPARATOR, PathBuf };

use clap::Parser;
use serde_json::{json, Map, Value};

use pacquet_package_json::{DependencyGroup, PackageJson};

use crate::package_manager::{PackageManager, PackageManagerError};

#[derive(Parser, Debug)]
pub struct ListArgs {
    /// Perform the command on every package subdirectories
    /// or on every workspace
    #[arg(long = "recursive", short = 'r')]
    pub recursive: bool,
    /// Log the output as JSON
    #[arg(long = "json")]
    pub json: bool,
    /// Show the extended information
    #[arg(long = "long")]
    pub long: bool,
    /// Outputs the package directories into a parseable format
    #[arg(long = "parseable")]
    pub parseable: bool,
    /// List the package in the global install directory instead of the
    /// current project
    #[arg(long = "global", short = 'g')]
    pub global: bool,
    /// Max display depth of the dependency tree
    #[arg(long = "depth")]
    pub depth: u32,
    /// Display only dependencies within dependencies or optionalDependencies
    #[arg(long = "prod", short = 'p')]
    pub prod: bool,
    /// Display only dependencies within devDependencies
    #[arg(long = "dev", short = 'd')]
    pub dev: bool,
    /// Omit packages from optionalDependencies
    #[arg(long = "no-optional")]
    pub no_opts: bool,
    /// Display only depndencies that are also projects within the workspace
    #[arg(long = "only-projects")]
    pub projects_only: bool,
    /// Display the dependencies from a given subset of dependencies
    #[arg(long = "filter", short = 'f')]
    pub filter: String,
}

impl ListArgs {
    pub fn get_scope(&self) -> DependencyGroup {
        if self.dev {
            DependencyGroup::Dev
        } else {
            DependencyGroup::Default
        }
    }

    pub fn get_depth(&self) -> u32 {
        let mut depth: u32 = 1;
        self.depth.clone_into(&mut depth);

        depth
    }
}

impl PackageManager {
    pub fn list(
        &self,
        package_json: &PackageJson,
        dependency_group: DependencyGroup,
        node_modules_path: &PathBuf,
        depth: u32,
    ) -> Result<String, PackageManagerError> {
        let mut scope: String = String::new();
        match dependency_group {
            DependencyGroup::Default => {
                let dependencies = package_json.get_dependencies(vec![DependencyGroup::Default]);
                
                for (name, version) in &dependencies {
                    scope = format!("{} - {}\n", name, version);

                    if depth > 1 {
                        let path = format!("{}{}{}", &name, MAIN_SEPARATOR, "package.json");
                        let pjson = PackageJson::from_path(&node_modules_path.join(path))?;
                        let n_dependencies = self.list(&pjson, DependencyGroup::Default, node_modules_path, depth - 1)?;
                        // TODO: fix format
                        scope = format!("\n\t{}", n_dependencies)
                    }
                }
            }
            DependencyGroup::Dev => scope = "dependencies".to_string(),
            _ => scope = "all".to_string(),
        }

        // let binding = Value::default();
        // let mut dependencies = self.value.get(scope).unwrap_or(&binding).as_object().into_iter();

        // let mut dep = dependencies.next();
        // while !dep.is_none() {
        //     println!("{:?}", dep);
        //     dep = dependencies.next();
        // }

        println!("{}", scope);
        Ok(scope.clone())
    }
}
