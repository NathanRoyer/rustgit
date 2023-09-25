use coolssh::{Connection, RunResult};
use lmfu::{HashSet, LiteMap};

use super::internals::{
    Result, Error, TcpStream, Write, Hash, Remote, Repository,
    GitProtocol, PacketLine, PackfileSender, dump_packfile_header,
};

impl Repository {
    /// Push committed changes upstream
    pub fn push(
        &mut self,
        remote: &Remote,
        updated_heads: &[(&str, Hash)],
        force_push: bool,
    ) -> Result<()> {
        let iter = updated_heads.iter().map(|(name, hash)| (*name, (*hash, Hash::zero())));
        let mut head_map = LiteMap::<&str, (Hash, Hash), Vec<_>>::from_iter(iter);

        let stream = TcpStream::connect(&*remote.host).unwrap();
        let auth = (&*remote.username, &*remote.keypair).into();
        let mut conn = Connection::new(stream, auth)?;

        conn.mutate_stream(|stream| {
            let duration = std::time::Duration::from_millis(1000);
            stream.set_read_timeout(Some(duration)).unwrap()
        });

        let command = format!("git-receive-pack {}", remote.path);
        let mut protocol = match conn.run(&command, &[])? {
            RunResult::Accepted(run) => GitProtocol::new(run),
            _ => panic!("run was refused"),
        };

        let mut _bytes = ByteCounter(0);
        let mut to_skip = HashSet::new();
        let mut thin_pack = false;
        let mut report_status = false;
        let mut client_caps = String::from("\0report-status");

        while let Some(line) = protocol.read_line_str()? {
            let line = match line.split_once('\0') {
                Some((line, server_caps)) => {
                    for cap in server_caps.split(' ') {
                        if cap == "thin-pack" {
                            client_caps += " thin-pack";
                            thin_pack = true;
                        }
                        if cap == "report-status" {
                            report_status = true;
                        }
                        log::debug!("PUSH-CAP: {}", cap);
                    }

                    line
                },
                None => line,
            };

            if let Some((hash_hex, ref_name)) = line.split_once(" refs/heads/") {
                let commit_hash = Hash::from_hex(hash_hex).ok_or(Error::GitProtocolError)?;
                if head_map.contains_key(ref_name) {
                    if force_push || self.objects.has(commit_hash) {
                        if let Some((_, old_hash)) = head_map.get_mut(ref_name) {
                            *old_hash = commit_hash;
                        }

                        if thin_pack {
                            self.objects.pack(commit_hash, &mut to_skip, &mut _bytes)?;
                        }
                    } else {
                        return Err(Error::MustForcePush);
                    }
                }
            }
        }

        if !report_status {
            log::error!("Remote server doesn't support report-status");
            return Err(Error::UnsupportedByRemote);
        }

        for (ref_name, (new_hash, old_hash)) in head_map.iter() {
            let line = format!("{} {} refs/heads/{}{}\n", old_hash, new_hash, ref_name, client_caps);
            client_caps.clear();

            protocol.write_lines(&[ PacketLine::String(&line) ])?;
        }

        protocol.write_lines(&[ PacketLine::FlushPacket ])?;

        let mut sender = PackfileSender::new(protocol);
        self.pack(to_skip, updated_heads, &mut sender, |_, _| ())?;
        let mut protocol = sender.finish()?;

        let fail = |got: &dyn core::fmt::Debug, expected| {
            log::error!("Unexpected line from remote: {:?} (was expecting {:?})", got, expected);
        };

        {
            let line = protocol.read_line_str()?;
            if line != Some("unpack ok") {
                fail(&line, "unpack ok");
                return Err(Error::GitProtocolError);
            }
        }

        while let Some(line) = protocol.read_line_str()? {
            if let Some(ref_name) = line.strip_prefix("ok refs/heads/") {
                head_map.remove(ref_name);
            } else {
                log::error!("Unexpected line from remote: {:?}", line);
                fail(&line, "ok refs/heads/{ref_name}");
                return Err(Error::GitProtocolError);
            }
        }

        if !head_map.is_empty() {
            log::error!("Remote forgot about: {:?}", head_map);
            return Err(Error::GitProtocolError);
        }

        // hmmm this may not always be correct
        self.upstream_head = self.head;

        Ok(())
    }

    pub fn pack<W: Write, F: Fn(&mut W, usize)>(
        &self,
        mut to_skip: HashSet<Hash>,
        heads_to_include: &[(&str, Hash)],
        dst: &mut W,
        size_hint: F,
    ) -> Result<()> {
        let (num_objects, bytes) = {
            let mut to_skip = to_skip.clone();
            let mut count = 0;
            let mut bytes = ByteCounter(0);

            for (_, commit_hash) in heads_to_include {
                count += self.objects.pack(*commit_hash, &mut to_skip, &mut bytes)?;
            }

            log::info!("Packfile: {} objects, {} bytes", count, bytes.0);
            (count, bytes.0)
        };

        size_hint(dst, crate::packfile::HEADER_SZ + bytes);
        dump_packfile_header(num_objects, dst);
        for (_, commit_hash) in heads_to_include {
            self.objects.pack(*commit_hash, &mut to_skip, dst)?;
        }

        Ok(())
    }
}

struct ByteCounter(usize);

impl Write for ByteCounter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let len = buf.len();
        self.0 += len;
        Ok(len)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
