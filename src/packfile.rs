use core::{str::from_utf8, mem::size_of};
use lmfu::HashSet;
use sha1::{Sha1, Digest};

use super::internals::{
    Result, Error, Write, ObjectStore, ObjectType, Hash,
    CommitField, GitProtocol, CommitParentsIter, TreeIter,
    get_commit_field_hash,
};

use miniz_oxide::inflate::{core::{DecompressorOxide, decompress, inflate_flags}, TINFLStatus};
use miniz_oxide::deflate::{core::{CompressorOxide, compress, deflate_flags, TDEFLStatus, TDEFLFlush}};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ObjectEncoding {
    Commit = 1,
    Tree = 2,
    Blob = 3,
    Tag = 4,
    OfsDelta = 6,
    RefDelta = 7,
}

impl TryFrom<u8> for ObjectEncoding {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Commit),
            2 => Ok(Self::Tree),
            3 => Ok(Self::Blob),
            4 => Ok(Self::Tag),
            6 => Ok(Self::OfsDelta),
            7 => Ok(Self::RefDelta),
            _ => Err(IPF),
        }
    }
}

#[derive(Clone, Debug)]
pub enum PackfileObject<T> {
    Commit(T), // 1
    Tree(T), // 2
    Blob(T), // 3
    Tag(T), // 4
    OfsDelta(T, usize), // 6
    RefDelta(T, Hash), // 7
}

const U32: usize = size_of::<u32>();
const SIG_V2: [u8; U32 + U32] = [b'P', b'A', b'C', b'K', 0, 0, 0, 2];
const BYTE_MSB: u8 = 0b1000_0000; // 0x80
const IPF: Error = Error::InvalidPackfile;
pub(crate) const HEADER_SZ: usize = U32 + U32 + U32;

pub struct PackfileReader<'a> {
    protocol: Option<GitProtocol<'a>>,
    pub out: Vec<u8>,
    buffer: Vec<u8>,
    num_objects: usize,
}

impl<'a> PackfileReader<'a> {
    pub fn new(protocol: GitProtocol<'a>) -> Result<PackfileReader<'a>> {
        Self::init(Self {
            protocol: Some(protocol),
            buffer: Vec::new(),
            out: Vec::new(),
            num_objects: 0,
        })
    }

    pub fn from_file(file: Vec<u8>) -> Result<PackfileReader<'a>> {
        Self::init(Self {
            protocol: None,
            buffer: file,
            out: Vec::new(),
            num_objects: 0,
        })
    }

    fn init(mut self) -> Result<PackfileReader<'a>> {
        loop {
            if self.buffer.len() >= HEADER_SZ {
                if self.buffer.starts_with(&SIG_V2) {
                    let mut u32_bytes = [0; U32];

                    u32_bytes.copy_from_slice(&self.buffer[SIG_V2.len()..][..U32]);
                    self.num_objects = u32::from_be_bytes(u32_bytes) as usize;

                    self.buffer.drain(0..HEADER_SZ);

                    break Ok(self);
                } else {
                    log::error!("Incorrect Packfile signature");
                    break Err(IPF);
                }
            } else {
                self.read_line()?;
            }
        }
    }

    // must not be called without expecting a line
    // returns buffer len
    fn read_line(&mut self) -> Result<usize> {
        let proto_error = Error::GitProtocolError;
        let protocol = self.protocol.as_mut().ok_or(IPF)?;
        match protocol.read_line()? {
            Some(bytes) => {
                let line_type = *bytes.get(0).ok_or(proto_error)?;
                let data = &bytes[1..];

                match line_type {
                    1 => {
                        self.buffer.extend_from_slice(data);
                        self.out.extend_from_slice(data);
                    },
                    2 => log::info!("Server Message: {}", from_utf8(data).ok().ok_or(proto_error)?),
                    _ => log::error!("Server Error: {}", from_utf8(data).ok().ok_or(proto_error)?),
                }

                match line_type == 0 || line_type > 2 {
                    true => Err(proto_error),
                    false => Ok(self.buffer.len()),
                }
            },
            None => Err(proto_error),
        }
    }

    pub fn num_objects(&self) -> usize {
        self.num_objects
    }

    fn read_size(&mut self) -> Result<(ObjectEncoding, usize)> {
        let mut i = 0;
        let mut size = 0;
        let mut shift = 0;

        loop {
            if let Some(byte) = self.buffer.get(i) {
                let (byte_mask, shift_inc) = match i {
                    0 => (0x0f, 4),
                    _ => (0x7f, 7),
                };

                checked_shift_add(*byte, &mut size, &mut shift, shift_inc, byte_mask, "Packfile object is too big")?;
                i += 1;

                if byte & BYTE_MSB == 0 {
                    let raw_type = (self.buffer[0] >> 4) & 0b111;
                    let enc_type = ObjectEncoding::try_from(raw_type)?;
                    self.buffer.drain(0..i);
                    break Ok((enc_type, size));
                }
            } else {
                self.read_line()?;
            }
        }
    }

    fn read_hash(&mut self) -> Result<Hash> {
        loop {
            if let Some(slice) = self.buffer.get(0..20) {
                let mut array = [0; 20];
                array.copy_from_slice(slice);
                self.buffer.drain(0..20);
                break Ok(Hash::new(array));
            } else {
                self.read_line()?;
            }
        }
    }

    pub fn next_object(&mut self) -> Result<PackfileObject<Box<[u8]>>> {
        let (encoding, size) = self.read_size()?;

        let hash = match encoding {
            ObjectEncoding::RefDelta => self.read_hash()?,
            _ => Hash::zero(),
        };

        log::trace!("Inflating a {:?} to {} bytes", encoding, size);

        let mut inflated = vec![0; size].into_boxed_slice();

        let flags = inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF
                  | inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER
                  | inflate_flags::TINFL_FLAG_COMPUTE_ADLER32;

        // todo: reuse the decompressor (advance inflated and drain input)

        let to_skip = loop {
            let new_decomp = &mut DecompressorOxide::new();

            match decompress(new_decomp, &*self.buffer, &mut inflated, 0, flags) {
                (TINFLStatus::Done, read, written) => match written == size {
                    true => break read,
                    false => (),
                },
                (TINFLStatus::FailedCannotMakeProgress, _, _) => (),
                e => {
                    log::error!("inflate() => {:?}", e);
                    return Err(IPF);
                },
            }

            self.read_line()?;
        };

        self.buffer.drain(0..to_skip);

        match encoding {
            ObjectEncoding::Commit => Ok(PackfileObject::Commit(inflated)),
            ObjectEncoding::Tree => Ok(PackfileObject::Tree(inflated)),
            ObjectEncoding::Blob => Ok(PackfileObject::Blob(inflated)),
            ObjectEncoding::Tag => Ok(PackfileObject::Tag(inflated)),
            ObjectEncoding::OfsDelta => Err(IPF),
            ObjectEncoding::RefDelta => Ok(PackfileObject::RefDelta(inflated, hash)),
        }
    }

    pub fn read_all_objects(&mut self, objects: &mut ObjectStore) -> Result<()> {
        let mut pending_delta = Vec::new();

        for _ in 0..self.num_objects {
            let object = self.next_object()?;

            if let PackfileObject::RefDelta(delta, hash) = object {
                if let Some(src) = objects.get(hash) {
                    let src_type = src.obj_type();
                    let dst = reconstruct(&delta, src.content())?;
                    let result_hash = objects.insert(src_type, dst, Some(hash));
                    log::trace!("Reconstructed {:>6} {}", src_type, result_hash);
                } else {
                    log::trace!("Missing delta source {}, will try again later", hash);
                    pending_delta.push((delta, hash));
                }
            } else {
                let (typ, hash) = match object {
                    PackfileObject::Commit(obj) => ("commit", objects.insert(ObjectType::Commit, obj, None)),
                    PackfileObject::Tree(obj) => ("tree", objects.insert(ObjectType::Tree, obj, None)),
                    PackfileObject::Blob(obj) => ("blob", objects.insert(ObjectType::Blob, obj, None)),
                    PackfileObject::Tag(obj) => ("tag", objects.insert(ObjectType::Tag, obj, None)),
                    _ => unreachable!(),
                };

                log::trace!("Inserted {:>11} {}", typ, hash);
            }
        }

        while !pending_delta.is_empty() {
            for i in 0..pending_delta.len() {
                let (delta, hash) = &pending_delta[i];
                if let Some(src) = objects.get(*hash) {
                    let src_type = src.obj_type();
                    let dst = reconstruct(&delta, src.content())?;
                    let result_hash = objects.insert(src_type, dst, Some(*hash));
                    pending_delta.remove(i);

                    log::trace!("Reconstructed {:>6} {}", src_type, result_hash);
                    break;
                }
            }

            log::error!("Can't reconstruct delta: missing objects");
            return Err(IPF);
        }

        Ok(())
    }
}

fn read_hdr_size(delta: &[u8], i: &mut usize) -> Result<usize> {
    let mut size = 0;
    let mut shift = 0;
    let mut byte = BYTE_MSB;

    while byte & BYTE_MSB > 0 {
        byte = *delta.get(*i).ok_or(IPF)?;
        checked_shift_add(byte, &mut size, &mut shift, 7, 0x7f, "Delta src/dst size is too big")?;
        *i += 1;
    }

    return Ok(size);
}

#[inline(always)]
fn checked_shift_add(src: u8, dst: &mut usize, shift: &mut usize, shift_inc: usize, src_mask: u8, errmsg: &str) -> Result<()> {
    let size_contrib = (src & src_mask) as usize;
    let shifted = size_contrib << *shift;
    let unshifted = shifted >> *shift;

    if unshifted != size_contrib {
        // we lost some bits due to a smaller CPU register size
        log::error!("{}", errmsg);
        Err(IPF)
    } else {
        *dst |= shifted;
        *shift += shift_inc;
        Ok(())
    }
}

fn reconstruct(delta: &[u8], src: &[u8]) -> Result<Box<[u8]>> {
    let mut i = 0;
    let _src_buf_size = read_hdr_size(&delta, &mut i)?;
    let dst_buf_size = read_hdr_size(&delta, &mut i)?;

    let mut dst = Vec::with_capacity(dst_buf_size);

    while let Some(instruction) = delta.get(i) {
        i += 1;

        if instruction & BYTE_MSB != 0 {
            // instruction: copy from base object
            log::trace!("Delta: COPY instruction");

            let mut offset = 0usize;
            for offset_byte in 0..4 {
                let has_offset_byte_n = instruction & (1 << offset_byte) != 0;

                if has_offset_byte_n {
                    let byte = *delta.get(i).ok_or(IPF)? as usize;
                    i += 1;

                    offset |= byte << (8 * offset_byte);
                }
            }

            let mut size = 0usize;
            for size_byte in 0..3 {
                let has_size_byte_n = instruction & (1 << (4 + size_byte)) != 0;

                if has_size_byte_n {
                    let byte = *delta.get(i).ok_or(IPF)? as usize;
                    i += 1;

                    size |= byte << (8 * size_byte);
                }
            }

            if size == 0 {
                if instruction & 0b01110000 > 0 {
                    log::warn!("Illegal size zero encoding in delta COPY instruction");
                }

                size = 0x1000;
            }

            let range = offset..(offset + size);
            let slice = src.get(range).ok_or(IPF)?;

            dst.extend_from_slice(slice);
        } else {
            // instruction: push new data
            log::trace!("Delta: PUSH instruction");

            let len = (instruction & 0x7f) as usize;
            let j = i + len;
            let slice = delta.get(i..j).ok_or(IPF)?;
            i = j;

            dst.extend_from_slice(slice);
        }
    }

    Ok(dst.into_boxed_slice())
}

fn write_encoding_size<W: Write>(mut size: usize, encoding: u8, dst: &mut W) {
    assert!(encoding < 8);

    let mut msb = size > 0xf;
    let byte = (size as u8 & 0xf) | (encoding << 4) | ((msb as u8) << 7);
    size >>= 4;
    dst.write(&[byte]).unwrap();

    while msb {
        let contrib = size as u8 & 0x7f;
        size >>= 7;
        msb = size != 0;
        let byte = contrib | ((msb as u8) << 7);
        dst.write(&[byte]).unwrap();
    }
}

pub fn dump_packfile_header<W: Write>(num_objects: usize, dst: &mut W) {
    dst.write(&SIG_V2).unwrap();
    dst.write(&(num_objects as u32).to_be_bytes()).unwrap();
}

pub fn dump_packfile_object<W: Write>(object: PackfileObject<&[u8]>, dst: &mut W) {
    use TDEFLStatus::*;

    let (inflated, hash, code) = match object {
        PackfileObject::Commit(bytes) => (bytes, None, 1),
        PackfileObject::Tree(bytes) => (bytes, None, 2),
        PackfileObject::Blob(bytes) => (bytes, None, 3),
        PackfileObject::Tag(bytes) => (bytes, None, 4),
        PackfileObject::OfsDelta(_, _) => unreachable!(),
        PackfileObject::RefDelta(bytes, hash) => (bytes, Some(hash), 7),
    };

    let size = inflated.len();

    write_encoding_size(size, code, dst);

    if let Some(hash) = hash {
        dst.write(&hash.to_bytes()).unwrap();
    }

    let flags = deflate_flags::TDEFL_COMPUTE_ADLER32
              | deflate_flags::TDEFL_FILTER_MATCHES
              | deflate_flags::TDEFL_WRITE_ZLIB_HEADER;

    let mut comp = CompressorOxide::new(flags);
    let mut to_deflate = &inflated[..];
    let mut buf = [0; 8096];

    loop {
        let flush = match to_deflate.is_empty() {
            true => TDEFLFlush::Finish,
            false => TDEFLFlush::None,
        };

        match compress(&mut comp, to_deflate, &mut buf, flush) {
            (Okay | PutBufFailed, in_progress, out_progress) => {
                dst.write(&buf[..out_progress]).unwrap();
                to_deflate = &to_deflate[in_progress..];
            },
            (Done, _, out_progress) => {
                dst.write(&buf[..out_progress]).unwrap();
                break;
            },
            e => panic!("deflate() => {:?}", e),
        };
    }
}

pub struct PackfileSender<'a> {
    protocol: GitProtocol<'a>,
    buffer: Vec<u8>,
    result: Result<()>,
    hasher: Sha1,
}

impl<'a> PackfileSender<'a> {
    pub fn new(protocol: GitProtocol<'a>) -> PackfileSender<'a> {
        Self {
            protocol,
            buffer: Vec::new(),
            result: Ok(()),
            hasher: Sha1::new(),
        }
    }

    pub fn finish(mut self) -> Result<GitProtocol<'a>> {
        let checksum: [u8; 20] = self.hasher.clone().finalize().into();
        self.buffer.extend_from_slice(&checksum);
        self.flush().unwrap();
        self.result?;
        Ok(self.protocol)
    }
}

const MAX: usize = 64000;

impl<'a> Write for PackfileSender<'a> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(buf);
        self.buffer.extend_from_slice(buf);
        if self.buffer.len() > MAX {
            self.flush()?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        for slice in self.buffer.chunks(MAX) {
            if let Ok(()) = self.result {
                self.result = self.protocol.write_raw(slice);
            } else {
                break;
            }
        }
        let len = self.buffer.len();
        self.buffer.drain(0..len);
        Ok(())
    }
}

impl ObjectStore {
    pub fn pack<W: Write>(&self, object: Hash, to_skip: &mut HashSet<Hash>, dst: &mut W) -> Result<usize> {
        if to_skip.contains_key(&object) {
            return Ok(0);
        }

        if !self.has(object) {
            // this is ok for shallow clones
            return Ok(0);
        }

        let mut count = 1;

        let entry = self.get(object).ok_or(Error::MissingObject)?;
        match entry.obj_type() {
            ObjectType::Commit => {
                let mut iter = CommitParentsIter::new(&entry.content());
                while let Some(hash) = iter.next()? {
                    count += self.pack(hash, to_skip, dst)?;
                }

                let tree = get_commit_field_hash(&entry.content(), CommitField::Tree)?;
                count += self.pack(tree.ok_or(Error::InvalidObject)?, to_skip, dst)?;
            },
            ObjectType::Tree => {
                let mut iter = TreeIter::new(&entry.content());
                while let Some((_, hash, _)) = iter.next()? {
                    count += self.pack(hash, to_skip, dst)?;
                }
            },
            ObjectType::Blob => (),
            ObjectType::Tag => (),
        }

        let raw_dump = true;
        if let Some(other_object) = entry.delta_hint() {
            if other_object != object {
                // todo
            } else {
                log::warn!("object's delta_hint was itself");
            }
        }

        if raw_dump {
            dump_packfile_object(match entry.obj_type() {
                ObjectType::Commit => PackfileObject::Commit(&entry.content()),
                ObjectType::Tree => PackfileObject::Tree(&entry.content()),
                ObjectType::Blob => PackfileObject::Blob(&entry.content()),
                ObjectType::Tag => PackfileObject::Tag(&entry.content()),
            }, dst);
        }

        to_skip.insert(object, ());

        Ok(count)
    }
}
