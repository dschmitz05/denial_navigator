//! Compatibility façade for the parser service's reusable X12 API.
//!
//! New code should depend on `denial_edi_core`, `denial_edi_835`, or
//! `denial_edi_837` directly. These re-exports keep fuzz targets and existing
//! integrations stable while the HTTP service remains in this crate.

pub mod common {
    pub use denial_edi_core::common::*;
}

pub mod schema {
    pub use denial_edi_core::schema::*;
}

pub mod x835 {
    pub use denial_edi_835::*;
}

pub mod x837 {
    pub use denial_edi_837::*;
}
