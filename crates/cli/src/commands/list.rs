use std::path::{PathBuf, MAIN_SEPARATOR};

use clap::Parser;
use termtree::Tree;

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
    ) -> Result<Tree<&str>, PackageManagerError> {
        match dependency_group {
            DependencyGroup::Default => {
                let mut root = Tree::new("");
                let dependencies = package_json.get_dependencies(vec![DependencyGroup::Default]);

                for (name, version) in dependencies {
                    // let label = format!("{} - {}", name, version);
                    // print!("{}", label);
                    let mut tree = helper::create_tree(name, version);

                    if depth > 1 {
                        let path = format!("{}{}{}", &name, MAIN_SEPARATOR, "package.json");
                        let pjson = PackageJson::from_path(&node_modules_path.join(path))?;
                        let subtree = self.list(
                            &pjson,
                            DependencyGroup::Default,
                            node_modules_path,
                            depth - 1,
                        )?;

                        tree.push(subtree);
                    }

                    root.push(tree.clone());
                }

                Ok(root)
            }
            // DependencyGroup::Dev => Ok(root),
            _ => Ok(Tree::new("".clone())),
        }
    }
}

mod helper {
    use termtree::Tree;
    pub fn create_tree<'a>(name: &'a str, version: &str) -> Tree<&'a str> {
        let label = format!("{}@{}", name.clone().to_owned(), version.clone().to_owned());
        Tree::new(label.to_owned().as_str())
    }
}
