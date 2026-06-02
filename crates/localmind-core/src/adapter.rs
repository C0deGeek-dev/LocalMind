use crate::{ContractResult, SessionRecord};

/// Maps a host-owned session representation into the neutral LocalMind contract.
///
/// Implementations live in host adapter crates. For example, Unshackled may
/// implement this trait for its exported session bundle type, but this crate
/// must not import or name that type directly.
pub trait HostSessionMapper<HostSession> {
    fn map_session(&self, host_session: &HostSession) -> ContractResult<SessionRecord>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostMappingRequirement {
    pub source_field: &'static str,
    pub localmind_field: &'static str,
    pub required: bool,
}
