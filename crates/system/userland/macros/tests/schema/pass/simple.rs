#![allow(unused)]
use selium_userland_macros::schema;

mod bindings {
    use core::marker::PhantomData;
    pub struct Echo<'a> {
        _phantom: PhantomData<&'a ()>,
    }
    pub struct EchoArgs<'a> {
        pub msg: Option<flatbuffers::WIPOffset<&'a str>>,
    }
    impl<'a> Default for EchoArgs<'a> {
        fn default() -> Self {
            Self { msg: None }
        }
    }
    impl<'a> Echo<'a> {
        pub fn create<'bldr, A>(
            _b: &mut flatbuffers::FlatBufferBuilder<'bldr, A>,
            _args: &EchoArgs<'bldr>,
        ) -> flatbuffers::WIPOffset<Self>
        where
            A: flatbuffers::Allocator,
        {
            unreachable!()
        }
    }

    impl<'a> Echo<'a> {
        pub fn msg(&self) -> Option<&'a str> {
            let _ = self;
            None
        }
    }

    impl<'a> flatbuffers::Follow<'a> for Echo<'a> {
        type Inner = Self;

        unsafe fn follow(_buf: &'a [u8], _loc: usize) -> Self::Inner {
            Self {
                _phantom: PhantomData,
            }
        }
    }

    impl<'a> flatbuffers::Verifiable for Echo<'a> {
        fn run_verifier(
            _v: &mut flatbuffers::Verifier,
            _pos: usize,
        ) -> Result<(), flatbuffers::InvalidFlatbuffer> {
            Ok(())
        }
    }
}

#[schema(
    path = concat!(env!("SEL_USERLAND_MACROS_DIR"), "/tests/schemas/echo.fbs"),
    ty = "selium.examples.Echo",
    binding = "crate::bindings::Echo"
)]
pub struct EchoMsg {
    msg: String,
}

fn main() {}
