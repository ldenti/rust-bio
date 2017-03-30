// Copyright 2014-2016 Johannes Köster, Christopher Schröder.
// Licensed under the MIT license (http://opensource.org/licenses/MIT)
// This file may not be copied, modified, or distributed
// except according to those terms.


//! FASTA format reading and writing.
//!
//! # Example
//!
//! ```
//! use std::io;
//! use bio::io::fasta;
//! let reader = fasta::Reader::new(io::stdin());
//! ```


use std::io;
use std::io::prelude::*;
use std::ascii::AsciiExt;
use std::collections;
use std::fs;
use std::path::Path;
use std::convert::AsRef;
use std::cmp::min;

use csv;

use utils::{TextSlice, Text};


/// Maximum size of temporary buffer used for reading indexed FASTA files.
const MAX_FASTA_BUFFER_SIZE: usize = 512;


/// A FASTA reader.
pub struct Reader<R: io::Read> {
    reader: io::BufReader<R>,
    line: String,
}


impl Reader<fs::File> {
    /// Read FASTA from given file path.
    pub fn from_file<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        fs::File::open(path).map(Reader::new)
    }
}


impl<R: io::Read> Reader<R> {
    /// Create a new Fasta reader given an instance of `io::Read`.
    pub fn new(reader: R) -> Self {
        Reader {
            reader: io::BufReader::new(reader),
            line: String::new(),
        }
    }

    /// Read next FASTA record into the given `Record`.
    pub fn read(&mut self, record: &mut Record) -> io::Result<()> {
        record.clear();
        if self.line.is_empty() {
            try!(self.reader.read_line(&mut self.line));
            if self.line.is_empty() {
                return Ok(());
            }
        }

        if !self.line.starts_with('>') {
            return Err(io::Error::new(io::ErrorKind::Other, "Expected > at record start."));
        }
        record.header.push_str(&self.line);
        loop {
            self.line.clear();
            try!(self.reader.read_line(&mut self.line));
            if self.line.is_empty() || self.line.starts_with('>') {
                break;
            }
            record.seq.push_str(self.line.trim_right());
        }

        Ok(())
    }

    /// Return an iterator over the records of this FastQ file.
    pub fn records(self) -> Records<R> {
        Records { reader: self }
    }
}


/// A FASTA index as created by SAMtools (.fai).
pub struct Index {
    inner: collections::HashMap<String, IndexRecord>,
    seqs: Vec<String>,
}


impl Index {
    /// Open a FASTA index from a given `io::Read` instance.
    pub fn new<R: io::Read>(fai: R) -> csv::Result<Self> {
        let mut inner = collections::HashMap::new();
        let mut seqs = vec![];
        let mut fai_reader = csv::Reader::from_reader(fai)
            .delimiter(b'\t')
            .has_headers(false);
        for row in fai_reader.decode() {
            let (name, record): (String, IndexRecord) = try!(row);
            seqs.push(name.clone());
            inner.insert(name, record);
        }
        Ok(Index {
               inner: inner,
               seqs: seqs,
           })
    }

    /// Open a FASTA index from a given file path.
    pub fn from_file<P: AsRef<Path>>(path: &P) -> csv::Result<Self> {
        match fs::File::open(path) {
            Ok(fai) => Self::new(fai),
            Err(e) => Err(csv::Error::Io(e)),
        }
    }

    /// Open a FASTA index given the corresponding FASTA file path (e.g. for ref.fasta we expect ref.fasta.fai).
    pub fn with_fasta_file<P: AsRef<Path>>(fasta_path: &P) -> csv::Result<Self> {
        let mut fai_path = fasta_path.as_ref().as_os_str().to_owned();
        fai_path.push(".fai");

        Self::from_file(&fai_path)
    }

    /// Return a vector of sequences described in the index.
    pub fn sequences(&self) -> Vec<Sequence> {
        self.seqs
            .iter()
            .map(|name| {
                     Sequence {
                         name: name.clone(),
                         len: self.inner[name].len,
                     }
                 })
            .collect()
    }
}


/// A FASTA reader with an index as created by SAMtools (.fai).
pub struct IndexedReader<R: io::Read + io::Seek> {
    reader: io::BufReader<R>,
    pub index: Index,
}


impl IndexedReader<fs::File> {
    /// Read from a given file path. This assumes the index ref.fasta.fai to be present for FASTA ref.fasta.
    pub fn from_file<P: AsRef<Path>>(path: &P) -> csv::Result<Self> {
        let index = try!(Index::with_fasta_file(path));

        match fs::File::open(path) {
            Ok(fasta) => Ok(IndexedReader::with_index(fasta, index)),
            Err(e) => Err(csv::Error::Io(e)),
        }
    }
}


impl<R: io::Read + io::Seek> IndexedReader<R> {
    /// Read from a FASTA and its index, both given as `io::Read`. FASTA has to be `io::Seek` in addition.
    pub fn new<I: io::Read>(fasta: R, fai: I) -> csv::Result<Self> {
        let index = try!(Index::new(fai));
        Ok(IndexedReader {
               reader: io::BufReader::new(fasta),
               index: index,
           })
    }

    /// Read from a FASTA and its index, the first given as `io::Read`, the second given as index object.
    pub fn with_index(fasta: R, index: Index) -> Self {
        IndexedReader {
            reader: io::BufReader::new(fasta),
            index: index,
        }
    }

    /// For a given seqname, read the whole sequence into the given vector.
    pub fn read_all(&mut self, seqname: &str, seq: &mut Text) -> io::Result<()> {
        let idx = self.idx(seqname)?;

        self.read_into_buffer(&idx, 0, idx.len, seq)
    }

    /// Read the given interval of the given seqname into the given vector (stop position is exclusive).
    pub fn read(&mut self, seqname: &str, start: u64, stop: u64, seq: &mut Text) -> io::Result<()> {
        let idx = self.idx(seqname)?;

        self.read_into_buffer(&idx, start, stop, seq)
    }


    /// For a given seqname, return an iterator yielding that sequence.
    pub fn read_iter_all(&mut self, seqname: &str)
                -> io::Result<IndexedReaderIterator<R>> {
        let idx = self.idx(seqname)?;

       self.read_into_iter(idx, 0, idx.len)
     }

    /// Read the given interval of the given seqname into the given vector (stop position is exclusive).
    pub fn read_iter(&mut self, seqname: &str, start: u64, stop: u64)
                -> io::Result<IndexedReaderIterator<R>> {
        let idx = self.idx(seqname)?;

        self.read_into_iter(idx, start, stop)
    }

    fn read_into_buffer(&mut self, idx: &IndexRecord, start: u64, stop: u64, seq: &mut Text) -> io::Result<()> {
        if stop > idx.len {
            return Err(io::Error::new(io::ErrorKind::Other,
                                      "FASTA read interval was out of bounds"));
        } else if start > stop {
            return Err(io::Error::new(io::ErrorKind::Other, "Invalid query interval"));
        }

        let mut bases_left = stop - start;
        let mut line_offset = self.seek_to(&idx, start)?;
        let mut buf = vec![0u8; Self::buffer_size(&idx, bases_left, line_offset)];

        seq.clear();
        while bases_left > 0 {
            let bases_read = self.read_line(&idx, &mut line_offset, bases_left, &mut buf)?;

            seq.extend_from_slice(&buf[..bases_read as usize]);
            bases_left -= bases_read;
        }

        Ok(())
    }

    fn read_into_iter(&mut self, idx: IndexRecord, start: u64, stop: u64)
                -> io::Result<IndexedReaderIterator<R>> {
        if stop > idx.len {
            return Err(io::Error::new(io::ErrorKind::Other,
                                      "FASTA read interval was out of bounds"));
        } else if start > stop {
            return Err(io::Error::new(io::ErrorKind::Other, "Invalid query interval"));
        }

        let line_offset = self.seek_to(&idx, start)?;

        Ok(IndexedReaderIterator {
            reader: self,
            record: idx,
            bases_left: stop - start,
            line_offset: line_offset,
            buf: vec![0u8; Self::buffer_size(&idx, stop - start, line_offset)],
            buf_len: 0,
            buf_idx: 0,
        })
    }

    /// Return the IndexRecord for the given sequence name or io::Result::Err
    fn idx(&self, seqname: &str) -> io::Result<IndexRecord> {
        match self.index.inner.get(seqname) {
            Some(idx) => Ok(idx.clone()),
            None => Err(io::Error::new(io::ErrorKind::Other, "Unknown sequence name.")),
        }
    }

    /// Seek to the given position in the specified FASTA record. The position
    /// of the cursor on the line that the seek ended on is returned.
    fn seek_to(&mut self, idx: &IndexRecord, start: u64) -> io::Result<u64> {
        assert!(start <= idx.len);

        let line_offset = start % idx.line_bases;
        let line_start = start / idx.line_bases * idx.line_bytes;
        let offset = idx.offset + line_start + line_offset;
        try!(self.reader.seek(io::SeekFrom::Start(offset)));

        Ok(line_offset)
    }

    /// Reads the remaining bases on the current line, but no more than bases_left
    /// nor any more than buf.len(). The actual number of bases read is returned.
    fn read_line(&mut self, idx: &IndexRecord, line_offset: &mut u64, bases_left: u64, buf: &mut [u8]) -> io::Result<u64> {
        let bases_on_line = idx.line_bases - min(idx.line_bases, *line_offset);

        let (bytes_to_read, bytes_to_keep) = if bases_on_line < bases_left {
            let bytes_to_read = min(buf.len() as u64, idx.line_bytes - *line_offset);
            let bytes_to_keep = min(bytes_to_read, bases_on_line);

            (bytes_to_read, bytes_to_keep)
        } else {
            let bytes_to_read = min(buf.len(), bases_left as usize) as u64;

            (bytes_to_read, bytes_to_read)
        };

        self.reader.read_exact(&mut buf[..bytes_to_read as usize])?;

        *line_offset += bytes_to_read;
        if *line_offset >= idx.line_bytes {
            *line_offset = 0;
        }

        Ok(bytes_to_keep)
    }

    /// Returns the buffer size to use. The calculation favors using up to the
    /// maximum number of bytes allowed, in order to prevent having to perform
    /// more than one read per line.
    fn buffer_size(idx: &IndexRecord, length: u64, line_offset: u64) -> usize {
        let buffer_size = if length < idx.line_bytes {
            if length + line_offset > idx.line_bases && length + line_offset < idx.line_bytes {
                // Ensure that we can read the first line in one go
                idx.line_bytes - line_offset
            } else {
                length
            }
        } else {
            idx.line_bytes
        };

        min(MAX_FASTA_BUFFER_SIZE, buffer_size as usize)
    }
}


/// Record of a FASTA index.
#[derive(RustcDecodable, Debug, Copy, Clone)]
struct IndexRecord {
    len: u64,
    offset: u64,
    line_bases: u64,
    line_bytes: u64,
}


/// A sequence record returned by the FASTA index.
pub struct Sequence {
    pub name: String,
    pub len: u64,
}


pub struct IndexedReaderIterator<'a, R: io::Read + io::Seek + 'a> {
    reader: &'a mut IndexedReader<R>,
    record: IndexRecord,
    bases_left: u64,
    line_offset: u64,
    buf: Vec<u8>,
    buf_idx: usize,
    buf_len: usize,
}


impl<'a, R: io::Read + io::Seek + 'a> IndexedReaderIterator<'a, R> {
    fn fill_buffer(&mut self) -> io::Result<()> {
        while self.buf_idx == self.buf_len {
            let bases_read = self.reader.read_line(&self.record, &mut self.line_offset, self.bases_left, &mut self.buf)?;

            self.buf_idx = 0;
            self.buf_len = bases_read as usize;
            self.bases_left -= bases_read;
        }

        Ok(())
    }
}


impl<'a, R: io::Read + io::Seek + 'a> Iterator for IndexedReaderIterator<'a, R> {
    type Item = io::Result<u8>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.buf_idx < self.buf_len {
            let item = Some(Ok(self.buf[self.buf_idx]));
            self.buf_idx += 1;
            item
        } else if self.bases_left > 0 {
            if let Err(e) = self.fill_buffer() {
                return Some(Err(e));
            }

            let item = Some(Ok(self.buf[self.buf_idx]));
            self.buf_idx += 1;
            item
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let hint = self.bases_left as usize + (self.buf_len - self.buf_idx);

        (hint, Some(hint))
    }
}


/// A Fasta writer.
pub struct Writer<W: io::Write> {
    writer: io::BufWriter<W>,
}


impl Writer<fs::File> {
    /// Write to the given file path.
    pub fn to_file<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        fs::File::create(path).map(Writer::new)
    }
}


impl<W: io::Write> Writer<W> {
    /// Create a new Fasta writer.
    pub fn new(writer: W) -> Self {
        Writer { writer: io::BufWriter::new(writer) }
    }

    /// Directly write a Fasta record.
    pub fn write_record(&mut self, record: &Record) -> io::Result<()> {
        self.write(record.id().unwrap_or(""), record.desc(), record.seq())
    }

    /// Write a Fasta record with given id, optional description and sequence.
    pub fn write(&mut self, id: &str, desc: Option<&str>, seq: TextSlice) -> io::Result<()> {
        try!(self.writer.write_all(b">"));
        try!(self.writer.write_all(id.as_bytes()));
        if desc.is_some() {
            try!(self.writer.write_all(b" "));
            try!(self.writer.write_all(desc.unwrap().as_bytes()));
        }
        try!(self.writer.write_all(b"\n"));
        try!(self.writer.write_all(seq));
        try!(self.writer.write_all(b"\n"));

        Ok(())
    }

    /// Flush the writer, ensuring that everything is written.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}


/// A FASTA record.
#[derive(Default)]
pub struct Record {
    header: String,
    seq: String,
}


impl Record {
    /// Create a new instance.
    pub fn new() -> Self {
        Record {
            header: String::new(),
            seq: String::new(),
        }
    }

    /// Check if record is empty.
    pub fn is_empty(&self) -> bool {
        self.header.is_empty() && self.seq.is_empty()
    }

    /// Check validity of Fasta record.
    pub fn check(&self) -> Result<(), &str> {
        if self.id().is_none() {
            return Err("Expecting id for FastQ record.");
        }
        if !self.seq.is_ascii() {
            return Err("Non-ascii character found in sequence.");
        }

        Ok(())
    }

    /// Return the id of the record.
    pub fn id(&self) -> Option<&str> {
        self.header[1..].trim_right().splitn(2, ' ').nth(0)
    }

    /// Return descriptions if present.
    pub fn desc(&self) -> Option<&str> {
        self.header[1..].trim_right().splitn(2, ' ').nth(1)
    }

    /// Return the sequence of the record.
    pub fn seq(&self) -> TextSlice {
        self.seq.as_bytes()
    }

    /// Clear the record.
    fn clear(&mut self) {
        self.header.clear();
        self.seq.clear();
    }
}


/// An iterator over the records of a Fasta file.
pub struct Records<R: io::Read> {
    reader: Reader<R>,
}


impl<R: io::Read> Iterator for Records<R> {
    type Item = io::Result<Record>;

    fn next(&mut self) -> Option<io::Result<Record>> {
        let mut record = Record::new();
        match self.reader.read(&mut record) {
            Ok(()) if record.is_empty() => None,
            Ok(()) => Some(Ok(record)),
            Err(err) => Some(Err(err)),
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    const FASTA_FILE: &'static [u8] = b">id desc
ACCGTAGGCTGA
CCGTAGGCTGAA
CGTAGGCTGAAA
GTAGGCTGAAAA
CCCC
>id2
ATTGTTGTTTTA
ATTGTTGTTTTA
ATTGTTGTTTTA
GGGG
";
    const FAI_FILE: &'static [u8] = b"id\t52\t9\t12\t13
id2\t40\t71\t12\t13
";

    const FASTA_FILE_CRLF: &'static [u8] = b">id desc\r
ACCGTAGGCTGA\r
CCGTAGGCTGAA\r
CGTAGGCTGAAA\r
GTAGGCTGAAAA\r
CCCC\r
>id2\r
ATTGTTGTTTTA\r
ATTGTTGTTTTA\r
ATTGTTGTTTTA\r
GGGG\r
";
    const FAI_FILE_CRLF: &'static [u8] = b"id\t52\t10\t12\t14\r
id2\t40\t78\t12\t14\r
";

    const FASTA_FILE_NO_TRAILING_LF: &'static [u8] = b">id desc
GTAGGCTGAAAA
CCCC";
    const FAI_FILE_NO_TRAILING_LF: &'static [u8] = b"id\t16\t9\t12\t13";


    const WRITE_FASTA_FILE: &'static [u8] = b">id desc
ACCGTAGGCTGA
>id2
ATTGTTGTTTTA
";

    #[test]
    fn test_reader() {
        let reader = Reader::new(FASTA_FILE);
        let ids = [Some("id"), Some("id2")];
        let descs = [Some("desc"), None];
        let seqs: [&[u8]; 2] = [b"ACCGTAGGCTGACCGTAGGCTGAACGTAGGCTGAAAGTAGGCTGAAAACCCC",
                                b"ATTGTTGTTTTAATTGTTGTTTTAATTGTTGTTTTAGGGG"];

        for (i, r) in reader.records().enumerate() {
            let record = r.ok().expect("Error reading record");
            assert_eq!(record.check(), Ok(()));
            assert_eq!(record.id(), ids[i]);
            assert_eq!(record.desc(), descs[i]);
            assert_eq!(record.seq(), seqs[i]);
        }
    }

    #[test]
    fn test_indexed_reader() {
        let mut reader = IndexedReader::new(io::Cursor::new(FASTA_FILE), FAI_FILE)
            .unwrap();

        _test_indexed_reader(&mut reader)
    }

    #[test]
    fn test_indexed_reader_crlf() {
        let mut reader = IndexedReader::new(io::Cursor::new(FASTA_FILE_CRLF), FAI_FILE_CRLF)
            .unwrap();

        _test_indexed_reader(&mut reader)
    }

    fn _test_indexed_reader<T: Seek + Read>(reader: &mut IndexedReader<T>) {
        let mut seq = Vec::new();

        // Test reading various substrings of the sequence
        reader.read("id", 1, 5, &mut seq).unwrap();
        assert_eq!(seq, b"CCGT");

        reader.read("id", 1, 31, &mut seq).unwrap();
        assert_eq!(seq, b"CCGTAGGCTGACCGTAGGCTGAACGTAGGC");

        reader.read("id", 13, 23, &mut seq).unwrap();
        assert_eq!(seq, b"CGTAGGCTGA");

        reader.read("id", 36, 52, &mut seq).unwrap();
        assert_eq!(seq, b"GTAGGCTGAAAACCCC");

        reader.read("id2", 12, 40, &mut seq).unwrap();
        assert_eq!(seq, b"ATTGTTGTTTTAATTGTTGTTTTAGGGG");

        reader.read("id2", 12, 12, &mut seq).unwrap();
        assert_eq!(seq, b"");

        reader.read("id2", 12, 13, &mut seq).unwrap();
        assert_eq!(seq, b"A");

        assert!(reader.read("id2", 12, 11, &mut seq).is_err());
        assert!(reader.read("id2", 12, 1000, &mut seq).is_err());
        assert!(reader.read("id3", 0, 1, &mut seq).is_err());
    }

    #[test]
    fn test_indexed_reader_no_trailing_lf() {
        let mut reader = IndexedReader::new(io::Cursor::new(FASTA_FILE_NO_TRAILING_LF),
                                            FAI_FILE_NO_TRAILING_LF)
                .unwrap();
        let mut seq = Vec::new();

        reader.read("id", 0, 16, &mut seq).unwrap();
        assert_eq!(seq, b"GTAGGCTGAAAACCCC");
    }

    #[test]
    fn test_writer() {
        let mut writer = Writer::new(Vec::new());
        writer.write("id", Some("desc"), b"ACCGTAGGCTGA").unwrap();
        writer.write("id2", None, b"ATTGTTGTTTTA").unwrap();
        writer.flush().unwrap();
        assert_eq!(writer.writer.get_ref(), &WRITE_FASTA_FILE);
    }
}
