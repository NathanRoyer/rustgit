use coolssh::{Connection, RunResult};

use super::internals::{
    Result, Error, Remote, PacketLine, GitProtocol,
    Hash, Repository, TcpStream, PackfileReader,
};

/// Specifies what to clone from a remote repository
#[derive(Debug)]
pub enum Reference<'a> {
    Head,
    Commit(Hash),
    Branch(&'a str),
}

use Reference::{Head, Branch};

impl Repository {
    /// Imports objects from a remote repository based on a reference
    ///
    /// Note: Can return `Err(GitProtocolError)` when an invalid Commit
    /// reference is specified (one which doesn't exist on the remote end).
    pub fn clone(
        &mut self,
        remote: &Remote,
        reference: Reference,
        depth: Option<usize>,
    ) -> Result<()> {
        let head_root = self.get_commit_root(self.head).unwrap();
        if self.upstream_head != self.head || (head_root.is_some() && head_root != self.root) {
            return Err(Error::DirtyWorkspace);
        }

        let stream = TcpStream::connect(&*remote.host).unwrap();
        let mut conn = Connection::new(stream, (&*remote.username, &*remote.keypair).into())?;

        conn.mutate_stream(|stream| {
            let duration = std::time::Duration::from_millis(1000);
            stream.set_read_timeout(Some(duration)).unwrap()
        });

        let env = [("GIT_PROTOCOL", "version=2")];

        let command = format!("git-upload-pack {}", remote.path);
        let gpe = Error::GitProtocolError;
        let mut protocol = match conn.run(&command, &env)? {
            RunResult::Accepted(run) => GitProtocol::new(run),
            _ => panic!("run was refused"),
        };

        let mut shallow_supported = false;
        while let Some(line) = protocol.read_line_str()? {
            log::debug!("Server capability: {}", line);
            if let Some(fetch_options) = line.strip_prefix("fetch=") {
                for option in fetch_options.split(' ') {
                    if option == "shallow" {
                        shallow_supported = true;
                    }
                }
            }
        }

        if let Reference::Commit(hash) = reference {
            self.head = hash;
        } else {
            self.head = Hash::zero();

            protocol.write_lines(&[
                PacketLine::String("command=ls-refs\n"),
                PacketLine::DelimiterPacket,
                PacketLine::FlushPacket,
            ])?;

            while let Some(line) = protocol.read_line_str()? {
                let (hash_hex, ref_name) = line.split_once(' ').ok_or(gpe)?;
                if let Head = reference {
                    if ref_name == "HEAD" {
                        self.head = Hash::from_hex(hash_hex).ok_or(gpe)?;
                        // don't break so that all lines are read
                    }
                } else if let Branch(branch) = reference {
                    if let Some(ref_name) = ref_name.strip_prefix("refs/heads/") {
                        if ref_name == branch {
                            self.head = Hash::from_hex(hash_hex).ok_or(gpe)?;
                            // don't break so that all lines are read
                        }
                    }
                }
            }

            if self.head == Hash::zero() {
                log::error!("Reference {:?} wasn't advertised by remote server", reference);
                return Err(Error::NoSuchReference);
            }
        }

        let want_head = format!("want {}", self.head);

        if let Some(num) = depth {
            if !shallow_supported {
                log::error!("Remote server doesn't support depth settings");
                return Err(Error::UnsupportedByRemote);
            }

            let deepen = format!("deepen {}", num);
            protocol.write_lines(&[
                PacketLine::String("command=fetch\n"),
                PacketLine::DelimiterPacket,
                PacketLine::String(&want_head),
                PacketLine::String("no-progress"),
                PacketLine::String(&deepen),
                // todo: thin-pack?
                PacketLine::String("done"),
                PacketLine::FlushPacket,
            ])?;
        } else {
            protocol.write_lines(&[
                PacketLine::String("command=fetch\n"),
                PacketLine::DelimiterPacket,
                PacketLine::String(&want_head),
                PacketLine::String("no-progress"),
                // todo: thin-pack?
                PacketLine::String("done"),
                PacketLine::FlushPacket,
            ])?;
        }

        while Some(b"packfile\n".as_slice()) != protocol.read_line()? {}

        let mut reader = PackfileReader::new(protocol)?;

        reader.read_all_objects(&mut self.objects)?;

        // todo: read footer

        self.upstream_head = self.head;
        self.root = self.get_commit_root(self.head)?;

        Ok(())
    }

    pub fn import_packfile(&mut self, packfile: Vec<u8>, head: Option<Hash>) -> Result<()> {
        let mut reader = PackfileReader::from_file(packfile)?;

        reader.read_all_objects(&mut self.objects)?;

        if let Some(head) = head {
            self.head = head;
            self.upstream_head = head;
            self.root = self.get_commit_root(head)?;
        }

        Ok(())
    }
}