//! Semantic actions for the rules in `grammar/statements/load.y`.

use super::prelude::*;

/// ```text
/// LoadStmt:
///       LOAD file_name
///     | opt_force INSTALL file_name opt_ext_version
///     | opt_force INSTALL file_name FROM ColId opt_ext_version
///     | opt_force INSTALL file_name FROM Sconst opt_ext_version
/// ```
pub(super) fn load_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // LOAD file_name
        0 => {
            let n = LoadStmt {
                filename: rhs.str(2)?,
                repository: None,
                repo_is_alias: false,
                version: None,
                load_type: LoadInstallType::Load,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // opt_force INSTALL file_name opt_ext_version
        1 => {
            let n = LoadStmt {
                filename: rhs.str(3)?,
                repository: None,
                repo_is_alias: false,
                version: rhs.str(4)?,
                load_type: rhs.load_install_type(1)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // opt_force INSTALL file_name FROM ColId opt_ext_version
        2 => {
            let n = LoadStmt {
                repository: rhs.str(5)?,
                repo_is_alias: true,
                filename: rhs.str(3)?,
                version: rhs.str(6)?,
                load_type: rhs.load_install_type(1)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        // opt_force INSTALL file_name FROM Sconst opt_ext_version
        3 => {
            let n = LoadStmt {
                filename: rhs.str(3)?,
                repository: rhs.str(5)?,
                repo_is_alias: false,
                version: rhs.str(6)?,
                load_type: rhs.load_install_type(1)?,
                ..Default::default()
            };
            Ok(SemValue::node(Node::from(n)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_force:
///       /* empty */
///     | FORCE
/// ```
pub(super) fn opt_force<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        0 => Ok(SemValue::LoadInstallType(LoadInstallType::Install)),
        // FORCE
        1 => Ok(SemValue::LoadInstallType(LoadInstallType::ForceInstall)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// file_name:
///       Sconst
///     | ColId
/// ```
pub(super) fn file_name<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // Sconst
        0 => Ok(SemValue::Str(rhs.str(1)?)),
        // ColId
        1 => Ok(SemValue::Str(rhs.str(1)?)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_ext_version:
///       /* empty */
///     | VERSION_P Sconst
///     | VERSION_P ColId
/// ```
pub(super) fn opt_ext_version<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        0 => Ok(SemValue::Str(None)),
        // VERSION_P Sconst
        1 => Ok(SemValue::Str(rhs.str(2)?)),
        // VERSION_P ColId
        2 => Ok(SemValue::Str(rhs.str(2)?)),
        _ => Err(rhs.bad_alternative()),
    }
}
