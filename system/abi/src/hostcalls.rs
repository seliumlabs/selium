//! Canonical catalogue of hostcall symbols shared between host and guest.
//!
//! The entries defined here are the single source of truth for:
//! - symbol names used in `#[link(wasm_import_module = "...")]`
//! - capability → hostcall coverage (for stub generation)
//! - input/output type pairing enforced at compile time

use core::marker::PhantomData;
use std::collections::BTreeMap;

use crate::{
    Capability, GuestResourceId, GuestUint, IoFrame, IoRead, IoWrite, NetAccept, NetAcceptReply,
    NetConnect, NetConnectReply, NetCreateListener, NetCreateListenerReply, NetTlsClientConfig,
    NetTlsConfigReply, NetTlsServerConfig, ProcessLogLookup, ProcessLogRegistration, ProcessStart,
    RkyvEncode, SessionCreate, SessionEntitlement, SessionRemove, SessionResource,
};

/// Type-erased metadata describing a hostcall.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct HostcallMeta {
    /// Wasm import module name.
    pub name: &'static str,
    /// Capability required to invoke the hostcall.
    pub capability: Capability,
}

/// Typed description of a hostcall linking point.
///
/// The generic parameters ensure that the host and guest agree on ABI payloads.
pub struct Hostcall<I, O> {
    meta: HostcallMeta,
    _marker: PhantomData<(I, O)>,
}

impl<I, O> Hostcall<I, O>
where
    I: RkyvEncode + Send,
    O: RkyvEncode + Send,
    for<'a> I::Archived: 'a
        + rkyv::Deserialize<I, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>
        + rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>,
    for<'a> O::Archived: 'a
        + rkyv::Deserialize<O, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>
        + rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>,
{
    /// Construct a new hostcall descriptor.
    pub const fn new(name: &'static str, capability: Capability) -> Self {
        Self {
            meta: HostcallMeta { name, capability },
            _marker: PhantomData,
        }
    }

    /// Access the symbol name.
    pub const fn name(&self) -> &'static str {
        self.meta.name
    }

    /// Access the required capability.
    pub const fn capability(&self) -> Capability {
        self.meta.capability
    }

    /// Access the type-erased metadata.
    pub const fn meta(&self) -> HostcallMeta {
        self.meta
    }
}

macro_rules! declare_hostcalls {
    (
        $( $ident:ident => {
            name: $name:literal,
            capability: $cap:path,
            input: $input:path,
            output: $output:ty
        }, )+
    ) => {
        $(
            #[doc = concat!("Hostcall descriptor for `", $name, "`.")]
            pub const $ident: Hostcall<$input, $output> = Hostcall::new($name, $cap);
        )+

        /// Complete catalogue of hostcalls, grouped by capability.
        pub const ALL: &[HostcallMeta] = &[
            $(HostcallMeta { name: $name, capability: $cap },)+
        ];

        /// Build a map of capabilities to the hostcalls they expose.
        pub fn by_capability() -> BTreeMap<Capability, Vec<&'static HostcallMeta>> {
            let mut map = BTreeMap::new();
            for meta in ALL {
                map.entry(meta.capability)
                    .or_insert_with(Vec::new)
                    .push(meta);
            }
            map
        }

        #[doc = "Expand to the canonical hostcall symbol name for the given identifier."]
        #[macro_export]
        macro_rules! hostcall_name {
            $(($ident) => { $name };)+
            ($other:ident) => {
                compile_error!(concat!("unknown hostcall: ", stringify!($other)))
            };
        }

        #[doc = "Expand to the typed hostcall descriptor for the given identifier."]
        #[macro_export]
        macro_rules! hostcall_contract {
            $(($ident) => { &$crate::hostcalls::$ident };)+
            ($other:ident) => {
                compile_error!(concat!("unknown hostcall: ", stringify!($other)))
            };
        }
    };
}

declare_hostcalls! {
    SESSION_CREATE => {
        name: "selium::session::create",
        capability: Capability::SessionLifecycle,
        input: SessionCreate,
        output: u32
    },
    SESSION_REMOVE => {
        name: "selium::session::remove",
        capability: Capability::SessionLifecycle,
        input: SessionRemove,
        output: ()
    },
    SESSION_ADD_ENTITLEMENT => {
        name: "selium::session::add_entitlement",
        capability: Capability::SessionLifecycle,
        input: SessionEntitlement,
        output: ()
    },
    SESSION_RM_ENTITLEMENT => {
        name: "selium::session::rm_entitlement",
        capability: Capability::SessionLifecycle,
        input: SessionEntitlement,
        output: ()
    },
    SESSION_ADD_RESOURCE => {
        name: "selium::session::add_resource",
        capability: Capability::SessionLifecycle,
        input: SessionResource,
        output: u32
    },
    SESSION_RM_RESOURCE => {
        name: "selium::session::rm_resource",
        capability: Capability::SessionLifecycle,
        input: SessionResource,
        output: u32
    },
    CHANNEL_CREATE => {
        name: "selium::channel::create",
        capability: Capability::ChannelLifecycle,
        input: GuestUint,
        output: GuestUint
    },
    CHANNEL_DELETE => {
        name: "selium::channel::delete",
        capability: Capability::ChannelLifecycle,
        input: GuestUint,
        output: ()
    },
    CHANNEL_DRAIN => {
        name: "selium::channel::drain",
        capability: Capability::ChannelLifecycle,
        input: u32,
        output: ()
    },
    CHANNEL_SHARE => {
        name: "selium::channel::share",
        capability: Capability::ChannelLifecycle,
        input: GuestUint,
        output: GuestResourceId
    },
    CHANNEL_ATTACH => {
        name: "selium::channel::attach",
        capability: Capability::ChannelLifecycle,
        input: GuestResourceId,
        output: GuestUint
    },
    CHANNEL_DETACH => {
        name: "selium::channel::detach",
        capability: Capability::ChannelLifecycle,
        input: GuestUint,
        output: ()
    },
    PROCESS_REGISTER_LOG => {
        name: "selium::process::register_log_channel",
        capability: Capability::ChannelLifecycle,
        input: ProcessLogRegistration,
        output: ()
    },
    CHANNEL_STRONG_READER_CREATE => {
        name: "selium::channel::strong_reader_create",
        capability: Capability::ChannelReader,
        input: GuestUint,
        output: GuestUint
    },
    CHANNEL_WEAK_READER_CREATE => {
        name: "selium::channel::weak_reader_create",
        capability: Capability::ChannelReader,
        input: GuestUint,
        output: GuestUint
    },
    CHANNEL_STRONG_READ => {
        name: "selium::channel::strong_read",
        capability: Capability::ChannelReader,
        input: IoRead,
        output: IoFrame
    },
    CHANNEL_WEAK_READ => {
        name: "selium::channel::weak_read",
        capability: Capability::ChannelReader,
        input: IoRead,
        output: IoFrame
    },
    CHANNEL_STRONG_WRITER_CREATE => {
        name: "selium::channel::strong_writer_create",
        capability: Capability::ChannelWriter,
        input: GuestUint,
        output: GuestUint
    },
    CHANNEL_WEAK_WRITER_CREATE => {
        name: "selium::channel::weak_writer_create",
        capability: Capability::ChannelWriter,
        input: GuestUint,
        output: GuestUint
    },
    CHANNEL_WRITER_DOWNGRADE => {
        name: "selium::channel::writer_downgrade",
        capability: Capability::ChannelWriter,
        input: GuestUint,
        output: GuestUint
    },
    CHANNEL_STRONG_WRITE => {
        name: "selium::channel::strong_write",
        capability: Capability::ChannelWriter,
        input: IoWrite,
        output: GuestUint
    },
    CHANNEL_WEAK_WRITE => {
        name: "selium::channel::weak_write",
        capability: Capability::ChannelWriter,
        input: IoWrite,
        output: GuestUint
    },
    PROCESS_LOG_CHANNEL => {
        name: "selium::process::log_channel",
        capability: Capability::ProcessLifecycle,
        input: ProcessLogLookup,
        output: GuestResourceId
    },
    PROCESS_START => {
        name: "selium::process::start",
        capability: Capability::ProcessLifecycle,
        input: ProcessStart,
        output: GuestResourceId
    },
    PROCESS_STOP => {
        name: "selium::process::stop",
        capability: Capability::ProcessLifecycle,
        input: GuestResourceId,
        output: ()
    },
    NET_QUIC_BIND => {
        name: "selium::net::quic::bind",
        capability: Capability::NetQuicBind,
        input: NetCreateListener,
        output: NetCreateListenerReply
    },
    NET_QUIC_ACCEPT => {
        name: "selium::net::quic::accept",
        capability: Capability::NetQuicAccept,
        input: NetAccept,
        output: NetAcceptReply
    },
    NET_QUIC_CONNECT => {
        name: "selium::net::quic::connect",
        capability: Capability::NetQuicConnect,
        input: NetConnect,
        output: NetConnectReply
    },
    NET_QUIC_READ => {
        name: "selium::net::quic::read",
        capability: Capability::NetQuicRead,
        input: IoRead,
        output: IoFrame
    },
    NET_QUIC_WRITE => {
        name: "selium::net::quic::write",
        capability: Capability::NetQuicWrite,
        input: IoWrite,
        output: GuestUint
    },
    NET_HTTP_BIND => {
        name: "selium::net::http::bind",
        capability: Capability::NetHttpBind,
        input: NetCreateListener,
        output: NetCreateListenerReply
    },
    NET_HTTP_ACCEPT => {
        name: "selium::net::http::accept",
        capability: Capability::NetHttpAccept,
        input: NetAccept,
        output: NetAcceptReply
    },
    NET_HTTP_CONNECT => {
        name: "selium::net::http::connect",
        capability: Capability::NetHttpConnect,
        input: NetConnect,
        output: NetConnectReply
    },
    NET_HTTP_READ => {
        name: "selium::net::http::read",
        capability: Capability::NetHttpRead,
        input: IoRead,
        output: IoFrame
    },
    NET_HTTP_WRITE => {
        name: "selium::net::http::write",
        capability: Capability::NetHttpWrite,
        input: IoWrite,
        output: GuestUint
    },
    NET_TLS_SERVER_CONFIG_CREATE => {
        name: "selium::net::tls::server_config_create",
        capability: Capability::NetTlsServerConfig,
        input: NetTlsServerConfig,
        output: NetTlsConfigReply
    },
    NET_TLS_CLIENT_CONFIG_CREATE => {
        name: "selium::net::tls::client_config_create",
        capability: Capability::NetTlsClientConfig,
        input: NetTlsClientConfig,
        output: NetTlsConfigReply
    },
}
