use crate::codec::compression::{
    CompressionEncoding, EnabledCompressionEncodings, SingleMessageCompressionOverride,
};
use crate::codec::{EncodeBody, Role};
use crate::metadata::GRPC_CONTENT_TYPE;
use crate::{
    body::BoxBody,
    client::GrpcService,
    codec::{Codec, Decoder, Streaming},
    request::SanitizeHeaders,
    Code, Request, Response, Status,
};
use http::{
    header::{HeaderValue, CONTENT_TYPE, TE},
    uri::{PathAndQuery, Uri},
};
use http_body::Body;
use std::{fmt, future, pin::pin};
use tokio_stream::{Stream, StreamExt};

/// A gRPC client dispatcher.
///
/// This will wrap some inner [`GrpcService`] and will encode/decode
/// messages via the provided codec.
///
/// Each request method takes a [`Request`], a [`PathAndQuery`], and a
/// [`Codec`]. The request contains the message to send via the
/// [`Codec::encoder`]. The path determines the fully qualified path
/// that will be append to the outgoing uri. The path must follow
/// the conventions explained in the [gRPC protocol definition] under `Path →`. An
/// example of this path could look like `/greeter.Greeter/SayHello`.
///
/// [gRPC protocol definition]: https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md#requests
pub struct Grpc<T> {
    inner: T,
    config: GrpcConfig,
}

struct GrpcConfig {
    origin: Uri,
    /// Which compression encodings does the client accept?
    accept_compression_encodings: EnabledCompressionEncodings,
    /// The compression encoding that will be applied to requests.
    send_compression_encodings: Option<CompressionEncoding>,
    /// Limits the maximum size of a decoded message.
    max_decoding_message_size: Option<usize>,
    /// Limits the maximum size of an encoded message.
    max_encoding_message_size: Option<usize>,
}

impl<T> Grpc<T> {
    /// Creates a new gRPC client with the provided [`GrpcService`].
    pub fn new(inner: T) -> Self {
        Self::with_origin(inner, Uri::default())
    }

    /// Creates a new gRPC client with the provided [`GrpcService`] and `Uri`.
    ///
    /// The provided Uri will use only the scheme and authority parts as the
    /// path_and_query portion will be set for each method.
    pub fn with_origin(inner: T, origin: Uri) -> Self {
        Self {
            inner,
            config: GrpcConfig {
                origin,
                send_compression_encodings: None,
                accept_compression_encodings: EnabledCompressionEncodings::default(),
                max_decoding_message_size: None,
                max_encoding_message_size: None,
            },
        }
    }

    /// Compress requests with the provided encoding.
    ///
    /// Requires the server to accept the specified encoding, otherwise it might return an error.
    ///
    /// # Example
    ///
    /// The most common way of using this is through a client generated by tonic-build:
    ///
    /// ```rust
    /// use tonic::transport::Channel;
    /// # enum CompressionEncoding { Gzip }
    /// # struct TestClient<T>(T);
    /// # impl<T> TestClient<T> {
    /// #     fn new(channel: T) -> Self { Self(channel) }
    /// #     fn send_compressed(self, _: CompressionEncoding) -> Self { self }
    /// # }
    ///
    /// # async {
    /// let channel = Channel::builder("127.0.0.1:3000".parse().unwrap())
    ///     .connect()
    ///     .await
    ///     .unwrap();
    ///
    /// let client = TestClient::new(channel).send_compressed(CompressionEncoding::Gzip);
    /// # };
    /// ```
    pub fn send_compressed(mut self, encoding: CompressionEncoding) -> Self {
        self.config.send_compression_encodings = Some(encoding);
        self
    }

    /// Enable accepting compressed responses.
    ///
    /// Requires the server to also support sending compressed responses.
    ///
    /// # Example
    ///
    /// The most common way of using this is through a client generated by tonic-build:
    ///
    /// ```rust
    /// use tonic::transport::Channel;
    /// # enum CompressionEncoding { Gzip }
    /// # struct TestClient<T>(T);
    /// # impl<T> TestClient<T> {
    /// #     fn new(channel: T) -> Self { Self(channel) }
    /// #     fn accept_compressed(self, _: CompressionEncoding) -> Self { self }
    /// # }
    ///
    /// # async {
    /// let channel = Channel::builder("127.0.0.1:3000".parse().unwrap())
    ///     .connect()
    ///     .await
    ///     .unwrap();
    ///
    /// let client = TestClient::new(channel).accept_compressed(CompressionEncoding::Gzip);
    /// # };
    /// ```
    pub fn accept_compressed(mut self, encoding: CompressionEncoding) -> Self {
        self.config.accept_compression_encodings.enable(encoding);
        self
    }

    /// Limits the maximum size of a decoded message.
    ///
    /// # Example
    ///
    /// The most common way of using this is through a client generated by tonic-build:
    ///
    /// ```rust
    /// use tonic::transport::Channel;
    /// # struct TestClient<T>(T);
    /// # impl<T> TestClient<T> {
    /// #     fn new(channel: T) -> Self { Self(channel) }
    /// #     fn max_decoding_message_size(self, _: usize) -> Self { self }
    /// # }
    ///
    /// # async {
    /// let channel = Channel::builder("127.0.0.1:3000".parse().unwrap())
    ///     .connect()
    ///     .await
    ///     .unwrap();
    ///
    /// // Set the limit to 2MB, Defaults to 4MB.
    /// let limit = 2 * 1024 * 1024;
    /// let client = TestClient::new(channel).max_decoding_message_size(limit);
    /// # };
    /// ```
    pub fn max_decoding_message_size(mut self, limit: usize) -> Self {
        self.config.max_decoding_message_size = Some(limit);
        self
    }

    /// Limits the maximum size of an encoded message.
    ///
    /// # Example
    ///
    /// The most common way of using this is through a client generated by tonic-build:
    ///
    /// ```rust
    /// use tonic::transport::Channel;
    /// # struct TestClient<T>(T);
    /// # impl<T> TestClient<T> {
    /// #     fn new(channel: T) -> Self { Self(channel) }
    /// #     fn max_encoding_message_size(self, _: usize) -> Self { self }
    /// # }
    ///
    /// # async {
    /// let channel = Channel::builder("127.0.0.1:3000".parse().unwrap())
    ///     .connect()
    ///     .await
    ///     .unwrap();
    ///
    /// // Set the limit to 2MB, Defaults to 4MB.
    /// let limit = 2 * 1024 * 1024;
    /// let client = TestClient::new(channel).max_encoding_message_size(limit);
    /// # };
    /// ```
    pub fn max_encoding_message_size(mut self, limit: usize) -> Self {
        self.config.max_encoding_message_size = Some(limit);
        self
    }

    /// Check if the inner [`GrpcService`] is able to accept a  new request.
    ///
    /// This will call [`GrpcService::poll_ready`] until it returns ready or
    /// an error. If this returns ready the inner [`GrpcService`] is ready to
    /// accept one more request.
    pub async fn ready(&mut self) -> Result<(), T::Error>
    where
        T: GrpcService<BoxBody>,
    {
        future::poll_fn(|cx| self.inner.poll_ready(cx)).await
    }

    /// Send a single unary gRPC request.
    pub async fn unary<M1, M2, C>(
        &mut self,
        request: Request<M1>,
        path: PathAndQuery,
        codec: C,
    ) -> Result<Response<M2>, Status>
    where
        T: GrpcService<BoxBody>,
        T::ResponseBody: Body + Send + 'static,
        <T::ResponseBody as Body>::Error: Into<crate::Error>,
        C: Codec<Encode = M1, Decode = M2>,
        M1: Send + Sync + 'static,
        M2: Send + Sync + 'static,
    {
        let request = request.map(|m| tokio_stream::once(m));
        self.client_streaming(request, path, codec).await
    }

    /// Send a client side streaming gRPC request.
    pub async fn client_streaming<S, M1, M2, C>(
        &mut self,
        request: Request<S>,
        path: PathAndQuery,
        codec: C,
    ) -> Result<Response<M2>, Status>
    where
        T: GrpcService<BoxBody>,
        T::ResponseBody: Body + Send + 'static,
        <T::ResponseBody as Body>::Error: Into<crate::Error>,
        S: Stream<Item = M1> + Send + 'static,
        C: Codec<Encode = M1, Decode = M2>,
        M1: Send + Sync + 'static,
        M2: Send + Sync + 'static,
    {
        let (mut parts, body, extensions) =
            self.streaming(request, path, codec).await?.into_parts();

        let mut body = pin!(body);

        let message = body
            .try_next()
            .await
            .map_err(|mut status| {
                status.metadata_mut().merge(parts.clone());
                status
            })?
            .ok_or_else(|| Status::internal("Missing response message."))?;

        if let Some(trailers) = body.trailers().await? {
            parts.merge(trailers);
        }

        Ok(Response::from_parts(parts, message, extensions))
    }

    /// Send a server side streaming gRPC request.
    pub async fn server_streaming<M1, M2, C>(
        &mut self,
        request: Request<M1>,
        path: PathAndQuery,
        codec: C,
    ) -> Result<Response<Streaming<M2>>, Status>
    where
        T: GrpcService<BoxBody>,
        T::ResponseBody: Body + Send + 'static,
        <T::ResponseBody as Body>::Error: Into<crate::Error>,
        C: Codec<Encode = M1, Decode = M2>,
        M1: Send + Sync + 'static,
        M2: Send + Sync + 'static,
    {
        let request = request.map(|m| tokio_stream::once(m));
        self.streaming(request, path, codec).await
    }

    /// Send a bi-directional streaming gRPC request.
    pub async fn streaming<S, M1, M2, C>(
        &mut self,
        request: Request<S>,
        path: PathAndQuery,
        mut codec: C,
    ) -> Result<Response<Streaming<M2>>, Status>
    where
        T: GrpcService<BoxBody>,
        T::ResponseBody: Body + Send + 'static,
        <T::ResponseBody as Body>::Error: Into<crate::Error>,
        S: Stream<Item = M1> + Send + 'static,
        C: Codec<Encode = M1, Decode = M2>,
        M1: Send + Sync + 'static,
        M2: Send + Sync + 'static,
    {
        let request = request
            .map(|s| {
                EncodeBody::new(
                    codec.encoder(),
                    s.map(Ok),
                    self.config.send_compression_encodings,
                    SingleMessageCompressionOverride::default(),
                    self.config.max_encoding_message_size,
                    Role::Client,
                )
            })
            .map(BoxBody::new);

        let request = self.config.prepare_request(request, path);

        let response = self
            .inner
            .call(request)
            .await
            .map_err(Status::from_error_generic)?;

        let decoder = codec.decoder();

        self.create_response(decoder, response)
    }

    // Keeping this code in a separate function from Self::streaming lets functions that return the
    // same output share the generated binary code
    fn create_response<M2>(
        &self,
        decoder: impl Decoder<Item = M2, Error = Status> + Send + 'static,
        response: http::Response<T::ResponseBody>,
    ) -> Result<Response<Streaming<M2>>, Status>
    where
        T: GrpcService<BoxBody>,
        T::ResponseBody: Body + Send + 'static,
        <T::ResponseBody as Body>::Error: Into<crate::Error>,
    {
        let encoding = CompressionEncoding::from_encoding_header(
            response.headers(),
            self.config.accept_compression_encodings,
        )?;

        let status_code = response.status();
        let trailers_only_status = Status::from_header_map(response.headers());

        // We do not need to check for trailers if the `grpc-status` header is present
        // with a valid code.
        let expect_additional_trailers = if let Some(status) = trailers_only_status {
            if status.code() != Code::Ok {
                return Err(status);
            }

            false
        } else {
            true
        };

        let response = response.map(|body| {
            if expect_additional_trailers {
                Streaming::new_response(
                    decoder,
                    body,
                    status_code,
                    encoding,
                    self.config.max_decoding_message_size,
                )
            } else {
                Streaming::new_empty(decoder, body)
            }
        });

        Ok(Response::from_http(response))
    }
}

impl GrpcConfig {
    fn prepare_request(
        &self,
        request: Request<BoxBody>,
        path: PathAndQuery,
    ) -> http::Request<BoxBody> {
        let mut parts = self.origin.clone().into_parts();

        match &parts.path_and_query {
            Some(pnq) if pnq != "/" => {
                parts.path_and_query = Some(
                    format!("{}{}", pnq.path(), path)
                        .parse()
                        .expect("must form valid path_and_query"),
                )
            }
            _ => {
                parts.path_and_query = Some(path);
            }
        }

        let uri = Uri::from_parts(parts).expect("path_and_query only is valid Uri");

        let mut request = request.into_http(
            uri,
            http::Method::POST,
            http::Version::HTTP_2,
            SanitizeHeaders::Yes,
        );

        // Add the gRPC related HTTP headers
        request
            .headers_mut()
            .insert(TE, HeaderValue::from_static("trailers"));

        // Set the content type
        request
            .headers_mut()
            .insert(CONTENT_TYPE, GRPC_CONTENT_TYPE);

        #[cfg(any(feature = "gzip", feature = "zstd"))]
        if let Some(encoding) = self.send_compression_encodings {
            request.headers_mut().insert(
                crate::codec::compression::ENCODING_HEADER,
                encoding.into_header_value(),
            );
        }

        if let Some(header_value) = self
            .accept_compression_encodings
            .into_accept_encoding_header_value()
        {
            request.headers_mut().insert(
                crate::codec::compression::ACCEPT_ENCODING_HEADER,
                header_value,
            );
        }

        request
    }
}

impl<T: Clone> Clone for Grpc<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            config: GrpcConfig {
                origin: self.config.origin.clone(),
                send_compression_encodings: self.config.send_compression_encodings,
                accept_compression_encodings: self.config.accept_compression_encodings,
                max_encoding_message_size: self.config.max_encoding_message_size,
                max_decoding_message_size: self.config.max_decoding_message_size,
            },
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Grpc<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut f = f.debug_struct("Grpc");

        f.field("inner", &self.inner);

        f.field("origin", &self.config.origin);

        f.field(
            "compression_encoding",
            &self.config.send_compression_encodings,
        );

        f.field(
            "accept_compression_encodings",
            &self.config.accept_compression_encodings,
        );

        f.field(
            "max_decoding_message_size",
            &self.config.max_decoding_message_size,
        );

        f.field(
            "max_encoding_message_size",
            &self.config.max_encoding_message_size,
        );

        f.finish()
    }
}
