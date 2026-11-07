#![cfg(target_os = "linux")]

mod atspi;
mod atspi_actions;
mod atspi_roles;
mod backend;
mod bus;
mod eis_client;
mod image;
#[cfg(feature = "pipewire")]
mod pipewire;
mod portal_bus;
mod x11;
mod x11_keysyms;

pub use backend::make_backend;
