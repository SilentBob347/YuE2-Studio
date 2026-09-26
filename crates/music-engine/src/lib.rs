//! Local engine supervision: the `yue-server` process from yue2.cpp and the
//! process group every child of the studio belongs to.

pub mod model;
pub mod process_group;
pub mod yue_server;
pub mod yue_train;
