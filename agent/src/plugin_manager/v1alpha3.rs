#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct NodePrepareResourcesRequest {
    /// The list of ResourceClaims that are to be prepared.
    #[prost(message, repeated, tag = "1")]
    pub claims: ::prost::alloc::vec::Vec<Claim>,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct NodePrepareResourcesResponse {
    /// The ResourceClaims for which preparation was done
    /// or attempted, with claim_uid as key.
    ///
    /// It is an error if some claim listed in NodePrepareResourcesRequest
    /// does not get prepared. NodePrepareResources
    /// will be called again for those that are missing.
    #[prost(map = "string, message", tag = "1")]
    pub claims:
        ::std::collections::HashMap<::prost::alloc::string::String, NodePrepareResourceResponse>,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct NodePrepareResourceResponse {
    /// These are the additional devices that kubelet must
    /// make available via the container runtime. A resource
    /// may have zero or more devices.
    #[prost(string, repeated, tag = "1")]
    pub cdi_devices: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
    /// If non-empty, preparing the ResourceClaim failed.
    /// cdi_devices is ignored in that case.
    #[prost(string, tag = "2")]
    pub error: ::prost::alloc::string::String,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct NodeUnprepareResourcesRequest {
    /// The list of ResourceClaims that are to be unprepared.
    #[prost(message, repeated, tag = "1")]
    pub claims: ::prost::alloc::vec::Vec<Claim>,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct NodeUnprepareResourcesResponse {
    /// The ResourceClaims for which preparation was reverted.
    /// The same rules as for NodePrepareResourcesResponse.claims
    /// apply.
    #[prost(map = "string, message", tag = "1")]
    pub claims:
        ::std::collections::HashMap<::prost::alloc::string::String, NodeUnprepareResourceResponse>,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct NodeUnprepareResourceResponse {
    /// If non-empty, unpreparing the ResourceClaim failed.
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Claim {
    /// The ResourceClaim namespace (ResourceClaim.meta.Namespace).
    /// This field is REQUIRED.
    #[prost(string, tag = "1")]
    pub namespace: ::prost::alloc::string::String,
    /// The UID of the Resource claim (ResourceClaim.meta.UUID).
    /// This field is REQUIRED.
    #[prost(string, tag = "2")]
    pub uid: ::prost::alloc::string::String,
    /// The name of the Resource claim (ResourceClaim.meta.Name)
    /// This field is REQUIRED.
    #[prost(string, tag = "3")]
    pub name: ::prost::alloc::string::String,
    /// Resource handle (AllocationResult.ResourceHandles\[*\].Data)
    /// This field is REQUIRED.
    #[prost(string, tag = "4")]
    pub resource_handle: ::prost::alloc::string::String,
}
/// Generated server implementations.
pub mod node_server {
    #![allow(unused_variables, dead_code, missing_docs, clippy::let_unit_value)]
    use tonic::codegen::*;
    /// Generated trait containing gRPC methods that should be implemented for use with NodeServer.
    #[async_trait]
    pub trait Node: Send + Sync + 'static {
        /// NodePrepareResources prepares several ResourceClaims
        /// for use on the node. If an error is returned, the
        /// response is ignored. Failures for individual claims
        /// can be reported inside NodePrepareResourcesResponse.
        async fn node_prepare_resources(
            &self,
            request: tonic::Request<super::NodePrepareResourcesRequest>,
        ) -> std::result::Result<tonic::Response<super::NodePrepareResourcesResponse>, tonic::Status>;
        /// NodeUnprepareResources is the opposite of NodePrepareResources.
        /// The same error handling rules apply,
        async fn node_unprepare_resources(
            &self,
            request: tonic::Request<super::NodeUnprepareResourcesRequest>,
        ) -> std::result::Result<
            tonic::Response<super::NodeUnprepareResourcesResponse>,
            tonic::Status,
        >;
    }
    #[derive(Debug)]
    pub struct NodeServer<T: Node> {
        inner: _Inner<T>,
        accept_compression_encodings: EnabledCompressionEncodings,
        send_compression_encodings: EnabledCompressionEncodings,
        max_decoding_message_size: Option<usize>,
        max_encoding_message_size: Option<usize>,
    }
    struct _Inner<T>(Arc<T>);
    impl<T: Node> NodeServer<T> {
        pub fn new(inner: T) -> Self {
            Self::from_arc(Arc::new(inner))
        }
        pub fn from_arc(inner: Arc<T>) -> Self {
            let inner = _Inner(inner);
            Self {
                inner,
                accept_compression_encodings: Default::default(),
                send_compression_encodings: Default::default(),
                max_decoding_message_size: None,
                max_encoding_message_size: None,
            }
        }
        pub fn with_interceptor<F>(inner: T, interceptor: F) -> InterceptedService<Self, F>
        where
            F: tonic::service::Interceptor,
        {
            InterceptedService::new(Self::new(inner), interceptor)
        }
        /// Enable decompressing requests with the given encoding.
        #[must_use]
        pub fn accept_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.accept_compression_encodings.enable(encoding);
            self
        }
        /// Compress responses with the given encoding, if the client supports it.
        #[must_use]
        pub fn send_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.send_compression_encodings.enable(encoding);
            self
        }
        /// Limits the maximum size of a decoded message.
        ///
        /// Default: `4MB`
        #[must_use]
        pub fn max_decoding_message_size(mut self, limit: usize) -> Self {
            self.max_decoding_message_size = Some(limit);
            self
        }
        /// Limits the maximum size of an encoded message.
        ///
        /// Default: `usize::MAX`
        #[must_use]
        pub fn max_encoding_message_size(mut self, limit: usize) -> Self {
            self.max_encoding_message_size = Some(limit);
            self
        }
    }
    impl<T, B> tonic::codegen::Service<http::Request<B>> for NodeServer<T>
    where
        T: Node,
        B: Body + Send + 'static,
        B::Error: Into<StdError> + Send + 'static,
    {
        type Response = http::Response<tonic::body::BoxBody>;
        type Error = std::convert::Infallible;
        type Future = BoxFuture<Self::Response, Self::Error>;
        fn poll_ready(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<std::result::Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn call(&mut self, req: http::Request<B>) -> Self::Future {
            let inner = self.inner.clone();
            match req.uri().path() {
                "/v1alpha3.Node/NodePrepareResources" => {
                    #[allow(non_camel_case_types)]
                    struct NodePrepareResourcesSvc<T: Node>(pub Arc<T>);
                    impl<T: Node> tonic::server::UnaryService<super::NodePrepareResourcesRequest>
                        for NodePrepareResourcesSvc<T>
                    {
                        type Response = super::NodePrepareResourcesResponse;
                        type Future = BoxFuture<tonic::Response<Self::Response>, tonic::Status>;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::NodePrepareResourcesRequest>,
                        ) -> Self::Future {
                            let inner = Arc::clone(&self.0);
                            let fut = async move {
                                <T as Node>::node_prepare_resources(&inner, request).await
                            };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let max_decoding_message_size = self.max_decoding_message_size;
                    let max_encoding_message_size = self.max_encoding_message_size;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = NodePrepareResourcesSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            )
                            .apply_max_message_size_config(
                                max_decoding_message_size,
                                max_encoding_message_size,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                "/v1alpha3.Node/NodeUnprepareResources" => {
                    #[allow(non_camel_case_types)]
                    struct NodeUnprepareResourcesSvc<T: Node>(pub Arc<T>);
                    impl<T: Node> tonic::server::UnaryService<super::NodeUnprepareResourcesRequest>
                        for NodeUnprepareResourcesSvc<T>
                    {
                        type Response = super::NodeUnprepareResourcesResponse;
                        type Future = BoxFuture<tonic::Response<Self::Response>, tonic::Status>;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::NodeUnprepareResourcesRequest>,
                        ) -> Self::Future {
                            let inner = Arc::clone(&self.0);
                            let fut = async move {
                                <T as Node>::node_unprepare_resources(&inner, request).await
                            };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let max_decoding_message_size = self.max_decoding_message_size;
                    let max_encoding_message_size = self.max_encoding_message_size;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = NodeUnprepareResourcesSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            )
                            .apply_max_message_size_config(
                                max_decoding_message_size,
                                max_encoding_message_size,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                _ => Box::pin(async move {
                    Ok(http::Response::builder()
                        .status(200)
                        .header("grpc-status", "12")
                        .header("content-type", "application/grpc")
                        .body(empty_body())
                        .unwrap())
                }),
            }
        }
    }
    impl<T: Node> Clone for NodeServer<T> {
        fn clone(&self) -> Self {
            let inner = self.inner.clone();
            Self {
                inner,
                accept_compression_encodings: self.accept_compression_encodings,
                send_compression_encodings: self.send_compression_encodings,
                max_decoding_message_size: self.max_decoding_message_size,
                max_encoding_message_size: self.max_encoding_message_size,
            }
        }
    }
    impl<T: Node> Clone for _Inner<T> {
        fn clone(&self) -> Self {
            Self(Arc::clone(&self.0))
        }
    }
    impl<T: std::fmt::Debug> std::fmt::Debug for _Inner<T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{:?}", self.0)
        }
    }
    impl<T: Node> tonic::server::NamedService for NodeServer<T> {
        const NAME: &'static str = "v1alpha3.Node";
    }
}
