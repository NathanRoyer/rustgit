#![doc = include_str!("../README.md")]

use coolssh::{Keypair, Error as SshError};
use std::{net::TcpStream, io::Write};

mod objectstore;
mod repository;
mod directory;
mod protocol;
mod packfile;
mod clone;
mod push;

pub use {repository::Repository, directory::{Mode, EntryType, FileType}, clone::Reference};

/// object store, directories, packfiles, git protocol
pub mod internals {
    pub(crate) use super::{
        TcpStream, Write, Remote, Result, Error, Repository,
        EntryType, FileType, Mode,
    };
    pub use {
        super::objectstore::{
            ObjectStore, Object, ObjectType, Hash, TreeIter, CommitParentsIter,
            CommitField, get_commit_field, get_commit_field_hash,
        },
        super::directory::{Directory, Path},
        super::protocol::{PacketLine, GitProtocol},
        super::packfile::{
            PackfileReader, PackfileObject, PackfileSender,
            dump_packfile_header, dump_packfile_object,
        },
    };
}

/// SSH & Remote Repository Settings
#[derive(Debug, Copy, Clone)]
pub struct Remote<'a> {
    /// `github.com:22`
    pub host: &'a str,
    /// `git`
    pub username: &'a str,
    /// `Username/Repository.git`
    pub path: &'a str,
    /// Must be registered at the remote
    pub keypair: &'a Keypair,
}

impl<'a> Remote<'a> {
    pub fn new(
        host: &'a str,
        username: &'a str,
        path: &'a str,
        keypair: &'a Keypair,
    ) -> Remote<'a> {
        Self {
            host,
            username,
            path,
            keypair,
        }
    }
}

/// Errors that can occur during repository manipulation
#[derive(Copy, Clone, Debug)]
pub enum Error {
    SshError(SshError),
    DirtyWorkspace,
    InvalidObject,
    PathError,
    MissingObject,
    GitProtocolError,
    InvalidPackfile,
    MustForcePush,
    UnsupportedByRemote,
}

impl From<SshError> for Error {
    fn from(ssh_error: SshError) -> Self {
        Self::SshError(ssh_error)
    }
}

/// `Result<T, Error>`
type Result<T> = core::result::Result<T, Error>;
