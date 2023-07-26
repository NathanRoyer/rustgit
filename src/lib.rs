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

fn init_logger() {
    use simplelog::*;
    let config = ConfigBuilder::new().set_location_level(LevelFilter::Off).build();
    let _ = SimpleLogger::init(LevelFilter::Info, config);
}

pub fn main() {
    init_logger();

    // ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG9CxFv9WFeHieOU9EsHXqzX1cT7YQPjlcn8wMlN2ZOf nathan.royer.pro@gmail.com

    let keypair = Keypair::from_bytes(&[
        229, 103, 222, 234, 170, 135, 159, 143,
        181, 187,  72, 156, 143, 178, 238, 187,
        187, 172, 117, 237, 230, 198, 174, 116,
         96,  35,  40, 192, 102, 198,  86, 137,
        111,  66, 196,  91, 253,  88,  87, 135,
        137, 227, 148, 244,  75,   7,  94, 172,
        215, 213, 196, 251,  97,   3, 227, 149,
        201, 252, 192, 201,  77, 217, 147, 159,
    ]).unwrap();

    let remote = Remote::new("github.com:22", "git", "NathanRoyer/rustgit.git", &keypair);

    let mut repo = Repository::new();
    repo.clone(remote, Reference::Branch("main"), Some(1)).unwrap();

    repo.stage("content.txt", Some(("Hello World!".into(), FileType::RegularFile))).unwrap();
    let new_head = repo.commit(
        "Tried a force push",
        ("Nathan Royer", "nathan.royer.pro@gmail.com"),
        ("Nathan Royer", "nathan.royer.pro@gmail.com"),
        None,
    ).unwrap();

    repo.push(remote, &[("abc", new_head)], true).unwrap();
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
