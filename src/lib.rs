#![doc = include_str!("../README.md")]

use std::{net::TcpStream, io::Write};
use lmfu::{json::{JsonFile, Path as JsonPath}, ArcStr};
pub use coolssh::{create_ed25519_keypair, dump_ed25519_pk_openssh, Error as SshError};

mod objectstore;
mod repository;
mod directory;
mod protocol;
mod packfile;
mod clone;
mod push;

pub use {
    repository::Repository, directory::{Mode, EntryType, FileType},
    clone::Reference, objectstore::Hash,
};

/// object store, directories, packfiles, git protocol
pub mod internals {
    pub(crate) use super::{
        TcpStream, Write, Remote, Result, Error, Repository,
        EntryType, FileType, Mode, Hash,
    };
    pub use {
        super::objectstore::{
            ObjectStore, Object, ObjectType, TreeIter, CommitParentsIter,
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
#[derive(Debug)]
pub struct Remote {
    /// `github.com:22`
    pub host: ArcStr,
    /// `git`
    pub username: ArcStr,
    /// `Username/Repository.git`
    pub path: ArcStr,
    /// Must be registered at the remote
    pub keypair: ArcStr,
}

impl Remote {
    pub fn new(
        host: ArcStr,
        username: ArcStr,
        path: ArcStr,
        keypair: ArcStr,
    ) -> Remote {
        Self {
            host,
            username,
            path,
            keypair,
        }
    }

    /// Reads remote access configuration from a [`JsonFile`]
    ///
    /// At `path`, the json file is expected to contain an
    /// object with the following keys:
    /// - `host`: SSH host (example: `github.com:22`)
    /// - `username`: SSH username (usually `git`)
    /// - `path`: path to the git repository
    /// - `keypair_hex`: 128-characters long hex-encoded key pair
    pub fn parse(json: &JsonFile, path: &JsonPath) -> core::result::Result<Self, &'static str> {
        let get = |prop, msg| json.get(&path.clone().i_str(prop)).as_string().ok_or(msg).cloned();
        let username = get("username", "Invalid username in json remote config")?;
        let keypair = get("keypair_hex", "Invalid keypair in json remote config")?;
        let host = get("host", "Invalid host in json remote config")?;
        let path = get("path", "Invalid path in json remote config")?;

        Ok(Self {
            host,
            username,
            path,
            keypair,
        })
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
    NoSuchReference,
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
