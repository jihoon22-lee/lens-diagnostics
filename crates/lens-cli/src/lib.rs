pub mod cli;
pub mod ops;
pub mod output;

// The doctor checks live in the shared operations layer; re-export the
// module so `lens_cli::doctor::…` keeps working for external users.
pub use lens_ops::doctor;

pub use output::flush as flush_stdout;
