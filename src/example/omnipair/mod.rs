//! same account layout for `swap`, same pair deserialization, quote math, and interest projection.

mod interest;
mod state;
mod venue;

pub use state::{
    DerivedAccounts, OmnipairPair, OMNIPAIR_PROGRAM_ID, SPL_TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID,
};
pub use venue::OmnipairVenue;
