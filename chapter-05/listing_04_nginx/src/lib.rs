use std::{marker::PhantomData, ptr::NonNull};

use nginx_sys::{
  ngx_buf_t, ngx_chain_t, ngx_http_output_filter, ngx_http_request_t,
  ngx_http_send_header, ngx_pcalloc, ngx_str_t, ngx_uint_t, off_t, size_t,
};

#[macro_export]
macro_rules! handler {
  ($c_fn_name:ident, $rs_fn_name:ident) => {
    #[no_mangle]
    pub unsafe extern "C" fn $c_fn_name(
      r: *mut nginx_sys::ngx_http_request_t,
    ) -> nginx_sys::ngx_int_t {
      let rc = nginx_sys::ngx_http_read_client_request_body(
        r,
        Some(read_body_handler),
      );
      if rc != 0 {
        return rc;
      }

      0
    }

    unsafe extern "C" fn read_body_handler(
      r: *mut nginx_sys::ngx_http_request_t,
    ) {
      let request = match std::ptr::NonNull::new(r) {
        Some(request) => request,
        None => {
          eprintln!("got null request in body handler");
          return;
        }
      };

      let request_body = $crate::RequestBody::new(request);

      let request = http::Request::builder().body(request_body).unwrap();

      let response = $rs_fn_name(request);

      if let Err(e) = $crate::write_response(r, response) {
        eprintln!("Failed to write NGINX response object: {}", e);
      }
    }
  };
}

pub struct RequestBody<'a> {
  lifetime: PhantomData<&'a ()>,
  request: NonNull<ngx_http_request_t>,
}

impl<'a> std::fmt::Debug for RequestBody<'a> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self.as_str() {
      Ok(s) => write!(f, "{:?}", s),
      Err(e) => write!(f, "<invalid request body: {}>", e),
    }
  }
}

impl<'a> RequestBody<'a> {
  pub fn new(request: NonNull<ngx_http_request_t>) -> Self {
    Self {
      request,
      lifetime: PhantomData,
    }
  }

  pub fn as_str(&self) -> Result<&'a str, &'static str> {
    unsafe {
      let request = self.request.as_ref();

      if request.request_body.is_null()
        || (*request.request_body).bufs.is_null()
        || (*(*request.request_body).bufs).buf.is_null()
      {
        return Err("Request body buffers were not initialized as expected");
      }

      let buf = (*(*request.request_body).bufs).buf;

      let start = (*buf).pos;
      let len = (*buf).last.offset_from(start) as usize;

      let body_bytes = std::slice::from_raw_parts(start, len);

      let body_str = std::str::from_utf8(body_bytes)
        .map_err(|_| "Body contains invalid UTF-8")?;

      Ok(body_str)
    }
  }
}

pub unsafe fn write_response(
  request: *mut ngx_http_request_t,
  response: http::Response<String>,
) -> Result<(), &'static str> {
  let headers = &mut (*request).headers_out;

  headers.status = response.status().as_u16() as ngx_uint_t;

  let response_bytes = response.body().as_bytes();
  headers.content_length_n = response_bytes.len() as off_t;

  let rc = ngx_http_send_header(request);
  if rc != 0 {
    return Err("failed to send headers");
  }

  let buf_p =
    ngx_pcalloc((*request).pool, std::mem::size_of::<ngx_buf_t>() as size_t)
      as *mut ngx_buf_t;
  if buf_p.is_null() {
    return Err("Failed to allocate buffer");
  }

  let buf = &mut (*buf_p);

  buf.set_last_buf(1);
  buf.set_last_in_chain(1);
  buf.set_memory(1);

  let response_buffer =
    ngx_pcalloc((*request).pool, response_bytes.len() as size_t);
  if response_buffer.is_null() {
    return Err("Failed to allocate response buffer");
  }

  std::ptr::copy_nonoverlapping(
    response_bytes.as_ptr(),
    response_buffer as *mut u8,
    response_bytes.len(),
  );

  buf.pos = response_buffer as *mut u8;
  buf.last = response_buffer.offset(response_bytes.len() as isize) as *mut u8;

  let mut out_chain = ngx_chain_t {
    buf,
    next: std::ptr::null_mut(),
  };

  if ngx_http_output_filter(request, &mut out_chain) != 0 {
    return Err("Failed to perform http output filter chain");
  }

  Ok(())
}
