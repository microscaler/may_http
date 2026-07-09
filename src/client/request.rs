//! Server Requests
//!
//! These are Requests sent by a `may_http::Server` to clients, after
//! receiving a request.
use std::cell::RefCell;
use std::fmt;
use std::io::{self, Write};
use std::ops::{Deref, DerefMut};
use std::rc::Rc;

// use http::header::*;
use crate::body::BodyWriter;
use http::{self, Method};

/// The outgoing half for a Stream, created by a `Client` and given to a `HttpClient`.
///
/// There is a `Drop` implementation for `Request` that will automatically
/// write the head and flush the body, if the handler has not already done so,
/// so that the client doesn't accidentally leave dangling requests.
///
/// it's a thin wraper to http::Request
/// impl Write for writing http Request body
pub struct Request {
    // the Raw http request
    raw_req: http::Request<BodyWriter>,
    // the underline write stream
    writer: Rc<RefCell<dyn Write>>,
    // the cached Request size
    body_size: Option<usize>,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "<HTTP Request {}>", self.method())
    }
}

impl Request {
    /// Creates a new Request that can be used to write to a network stream.
    #[inline]
    pub fn new(stream: Rc<RefCell<dyn Write>>) -> Request {
        Request {
            raw_req: http::Request::new(BodyWriter::InvalidWriter),
            writer: stream,
            body_size: None,
        }
    }

    // actual write head to stream
    fn write_head_impl(&mut self) -> io::Result<()> {
        let mut writer = self.writer.borrow_mut();

        write!(
            writer,
            "{} {} {:?}\r\n",
            self.method(),
            self.uri(),
            self.version()
        )?;
        // TODO: check server header
        write!(writer, "User-Agent: may_http\r\nAccept: */*\r\n")?;

        for (key, value) in self.headers().iter() {
            write!(
                writer,
                "{}: {}\r\n",
                key.as_str(),
                value.to_str().unwrap_or("")
            )?;
        }

        if let Some(len) = self.body_size {
            write!(writer, "Content-Length: {}\r\n", len)?
        }

        write!(writer, "\r\n")?;
        Ok(())
    }

    // write head to stream
    fn write_head(&mut self) -> io::Result<BodyWriter> {
        let body = match *self.method() {
            Method::GET | Method::HEAD => BodyWriter::EmptyWriter(self.writer.clone()),
            Method::POST => match self.body_size {
                Some(size) => BodyWriter::SizedWriter(self.writer.clone(), size),
                None => BodyWriter::ChunkWriter(self.writer.clone()),
            },
            // DELETE / PUT / PATCH / OPTIONS etc. Previously `unimplemented!()`,
            // which panicked in `Drop` for any non-GET/HEAD/POST request (e.g.
            // an HTTP proxy forwarding DELETE). A known body size gets a sized
            // writer; without one we assume no body — callers writing a body of
            // unknown length must call `set_content_length` first, since we do
            // not emit a Transfer-Encoding header for these methods.
            _ => match self.body_size {
                Some(size) => BodyWriter::SizedWriter(self.writer.clone(), size),
                None => BodyWriter::EmptyWriter(self.writer.clone()),
            },
        };
        self.write_head_impl()?;
        Ok(body)
    }

    /// Writes the body and ends the Request.
    ///
    /// This is a shortcut method for when you have a Request with a fixed
    /// size, and would only need a single `write` call normally.
    /// successive write would return Ok(0) or write error becuase the writer
    /// is closed.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use may_http::client::Request;
    /// fn handler(req: &mut Request) {
    ///     req.send(b"Hello World!").unwrap();
    /// }
    /// ```
    ///
    /// The above is the same, but shorter, than the longer:
    ///
    /// ```no_run
    /// # use may_http::client::Request;
    /// use std::io::Write;
    /// fn handler(mut req: Request) {
    ///     let body = b"Hello World!";
    ///     req.set_content_length(body.len());
    ///     req.write_all(body).unwrap();
    /// }
    /// ```
    #[inline]
    pub fn send(&mut self, body: &[u8]) -> io::Result<()> {
        self.body_size = Some(body.len());
        self.write_all(body)
    }

    /// set the content-length
    ///
    /// if you don't call `send()`, should call this before write the Request
    #[inline]
    pub fn set_content_length(&mut self, len: usize) {
        self.body_size = Some(len);
    }

    /// get the connection

    pub(super) fn conn(&self) -> &Rc<RefCell<dyn Write>> {
        &self.writer
    }
}

impl Deref for Request {
    type Target = http::Request<BodyWriter>;

    /// deref to the http::Request
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.raw_req
    }
}

impl DerefMut for Request {
    /// deref_mut to the http::Request
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.raw_req
    }
}

impl Write for Request {
    #[inline]
    fn write(&mut self, msg: &[u8]) -> io::Result<usize> {
        if let BodyWriter::InvalidWriter = *self.body() {
            *self.body_mut() = self.write_head()?;
        }
        self.body_mut().write(msg)
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        use std::thread;

        if thread::panicking() {
            // just let it panick
            return;
        }

        // make sure we write every thing
        if let BodyWriter::InvalidWriter = *self.body() {
            *self.body_mut() = self
                .write_head()
                .unwrap_or_else(|_| BodyWriter::EmptyWriter(self.writer.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with_method(method: Method, stream: Rc<RefCell<Vec<u8>>>) -> Request {
        let mut req = Request::new(stream);
        *req.method_mut() = method;
        *req.uri_mut() = "/things/42".parse().unwrap();
        req
    }

    fn written(stream: &Rc<RefCell<Vec<u8>>>) -> String {
        String::from_utf8(stream.borrow().clone()).unwrap()
    }

    // Regression: write_head hit `unimplemented!()` for any method other than
    // GET/HEAD/POST, panicking inside Drop (e.g. a proxy forwarding DELETE).
    #[test]
    fn delete_without_body_writes_head_on_drop() {
        let stream: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
        let req = request_with_method(Method::DELETE, stream.clone());
        drop(req);
        let head = written(&stream);
        assert!(head.starts_with("DELETE /things/42"), "head was: {head}");
        assert!(!head.contains("Content-Length"), "head was: {head}");
    }

    #[test]
    fn put_with_sized_body_writes_content_length() {
        let stream: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
        let mut req = request_with_method(Method::PUT, stream.clone());
        req.send(b"{\"a\":1}").unwrap();
        drop(req);
        let head = written(&stream);
        assert!(head.starts_with("PUT /things/42"), "head was: {head}");
        assert!(head.contains("Content-Length: 7"), "head was: {head}");
        assert!(head.ends_with("{\"a\":1}"), "head was: {head}");
    }

    #[test]
    fn patch_and_options_do_not_panic() {
        for method in [Method::PATCH, Method::OPTIONS] {
            let stream: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
            let req = request_with_method(method.clone(), stream.clone());
            drop(req);
            assert!(
                written(&stream).starts_with(method.as_str()),
                "no head written for {method}"
            );
        }
    }
}
