use crate::core::platform::linux::Errno;
use crate::core::platform::linux::ScmpAction;

#[derive(Clone, Copy, Debug)]
pub enum DisobeyPolicy {
    KillProcess,
    NoPerm,
}

impl Into<ScmpAction> for DisobeyPolicy {
    fn into(self) -> ScmpAction {
        match self {
            Self::KillProcess => ScmpAction::KillProcess,
            Self::NoPerm => ScmpAction::Errno(Errno::EPERM as i32),
        }
    }
}

impl From<ScmpAction> for DisobeyPolicy {
    fn from(value: ScmpAction) -> Self {
        match value {
            ScmpAction::KillProcess => Self::KillProcess,
            _ => Self::NoPerm,
        }
    }
}
