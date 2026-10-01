//! Native gRPC client for the bounded local store authority. No local data fallback.

use std::time::{Duration, Instant};

use prost::Message;
use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};
use tonic::transport::Endpoint;

pub mod protocol {
    tonic::include_proto!("horizon.store.v1");
}

pub use protocol::{GetRenderContextRequest, GetRenderContextResponse};
pub type ClientResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const MAX_CONTEXT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_REQUEST_BYTES: usize = 8 * 1024;
pub const THEME_SHA: &str = "5acd1b6b66c02f61d3216e3adace5dd9e0404fc9";

pub struct Snapshot {
    pub context_json: Vec<u8>,
    pub metadata: Json,
}

pub fn validate_query(query: &GetRenderContextRequest, timeout: Duration) -> ClientResult<()> {
    if timeout.is_zero() || timeout > Duration::from_secs(10) {
        return Err("RPC timeout must be positive and at most 10 seconds".into());
    }
    if query.encoded_len() > MAX_REQUEST_BYTES
        || query.tenant_id.is_empty()
        || query.request_id.is_empty()
        || query.current_page == 0
        || !["small", "large"].contains(&query.storefront_id.as_str())
        || !["en", "de", "pl"].contains(&query.locale.as_str())
        || !["baseline", "published", "default", "wide"].contains(&query.configuration.as_str())
        || ![
            "index",
            "product",
            "collection",
            "collection_page_2",
            "collection_last",
        ]
        .contains(&query.page.as_str())
        || !["default", "empty"].contains(&query.cart_id.as_str())
    {
        return Err("Invalid or oversized scoped store request".into());
    }
    Ok(())
}

pub fn validate_snapshot(
    query: &GetRenderContextRequest,
    reply: &GetRenderContextResponse,
) -> ClientResult<Json> {
    if reply.tenant_id != query.tenant_id
        || reply.storefront_id != query.storefront_id
        || reply.locale != query.locale
        || reply.configuration != query.configuration
        || reply.page != query.page
        || reply.current_page != query.current_page
        || reply.cart_id != query.cart_id
        || reply.request_id != query.request_id
    {
        return Err("RPC response scope differs from the authenticated request".into());
    }
    if reply.snapshot_revision.is_empty()
        || reply.snapshot_revision.len() > 128
        || !reply
            .snapshot_revision
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        || reply.context_json.is_empty()
        || reply.context_json.len() > MAX_CONTEXT_BYTES
    {
        return Err("RPC snapshot revision or context size is invalid".into());
    }
    let digest = format!("{:x}", Sha256::digest(&reply.context_json));
    if digest != reply.context_sha256 {
        return Err("RPC context SHA256 differs from the authoritative bytes".into());
    }
    let context: Json = serde_json::from_slice(&reply.context_json)?;
    if context["synthetic"] != true
        || context["schema_version"] != 1
        || !context["pages"][&query.page].is_object()
        || context["globals"]["request"]["locale"]["iso_code"] != query.locale
        || context["globals"]["localization"]["language"]["iso_code"] != query.locale
    {
        return Err("RPC context schema, selected page, or locale differs".into());
    }
    let page = &context["pages"][&query.page];
    let page_type = match query.page.as_str() {
        "index" => "index",
        "product" => "product",
        _ => "collection",
    };
    if context["theme"]["sha"] != THEME_SHA
        || page["template"] != format!("templates/{page_type}.json")
        || page.get("type").is_some_and(|kind| kind != page_type)
    {
        return Err("RPC context theme pin or selected page template/type differs".into());
    }
    let current_page = match page
        .get("current_page")
        .or_else(|| context["globals"].get("current_page"))
    {
        Some(value) => value
            .as_u64()
            .ok_or("RPC current_page must be a positive integer")?,
        None => 1,
    };
    if current_page != u64::from(query.current_page) {
        return Err("RPC context current_page differs from the authoritative request".into());
    }
    let legacy_baseline = query.configuration == "baseline"
        && query.tenant_id == "demo-a"
        && query.storefront_id == "small"
        && query.locale == "en"
        && query.page == "index"
        && query.current_page == 1
        && query.cart_id == "default"
        && digest == "867c41e0929881290af2b261af64146632bf0287524f5a6c55714af32e819f98";
    if query.configuration == "baseline" && !legacy_baseline {
        return Err("RPC baseline context differs from its immutable scope and pin".into());
    }
    if !legacy_baseline {
        if context["theme"]["configuration_source"] != "service"
            || !context["theme"]["settings"].is_object()
        {
            return Err(
                "RPC context must declare service-authoritative theme configuration".into(),
            );
        }
        let expected = json!({"tenant_id":query.tenant_id,"storefront_id":query.storefront_id,"locale":query.locale,"configuration":query.configuration,"cart_id":query.cart_id,"page":query.page,"current_page":query.current_page});
        if !expected
            .as_object()
            .unwrap()
            .iter()
            .all(|(key, value)| context["manifest"]["service_scope"][key] == *value)
            || page["type"] != page_type
        {
            return Err("RPC JSON service scope differs from response metadata".into());
        }
    }
    Ok(context)
}

pub async fn fetch(
    endpoint: &str,
    token: &str,
    query: GetRenderContextRequest,
    timeout: Duration,
) -> ClientResult<Snapshot> {
    validate_query(&query, timeout)?;
    let endpoint = Endpoint::from_shared(endpoint.to_owned())?;
    let uri = endpoint.uri().clone();
    if uri.scheme_str() != Some("http")
        || !matches!(uri.host(), Some("127.0.0.1" | "[::1]" | "::1"))
        || uri.port_u16().is_none()
        || uri
            .authority()
            .is_some_and(|authority| authority.as_str().contains('@'))
        || uri.query().is_some()
        || !["", "/"].contains(&uri.path())
    {
        return Err("Mock gRPC transport requires an explicit loopback HTTP endpoint".into());
    }
    if token.is_empty() {
        return Err("RPC authorization token is required".into());
    }
    let authorization = format!("Bearer {token}").parse()?;
    let started = Instant::now();
    let reply = tokio::time::timeout(timeout, async {
        let channel = endpoint
            .connect_timeout(timeout)
            .timeout(timeout)
            .connect()
            .await?;
        let mut client =
            protocol::store_context_service_client::StoreContextServiceClient::new(channel)
                .max_decoding_message_size(MAX_CONTEXT_BYTES + MAX_REQUEST_BYTES)
                .max_encoding_message_size(MAX_REQUEST_BYTES);
        let mut request = tonic::Request::new(query.clone());
        request
            .metadata_mut()
            .insert("authorization", authorization);
        request.set_timeout(timeout);
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
            client.get_render_context(request).await?.into_inner(),
        )
    })
    .await
    .map_err(|_| "RPC deadline exceeded; no local fallback")??;
    let fetch_ms = started.elapsed().as_secs_f64() * 1000.0;
    validate_snapshot(&query, &reply)?;
    let metadata = json!({"schema_version":1,"transport":"native tonic gRPC","endpoint":endpoint_text(&uri),"fetch_ms":fetch_ms,"timeout_ms":timeout.as_millis(),"response_cache":false,"context_sha256":reply.context_sha256,"context_bytes":reply.context_json.len(),"snapshot_revision":reply.snapshot_revision,"scope":{"tenant_id":reply.tenant_id,"storefront_id":reply.storefront_id,"locale":reply.locale,"configuration":reply.configuration,"page":reply.page,"current_page":reply.current_page,"cart_id":reply.cart_id,"request_id":reply.request_id},"correctness_verified":true});
    Ok(Snapshot {
        context_json: reply.context_json,
        metadata,
    })
}

fn endpoint_text(uri: &tonic::codegen::http::Uri) -> String {
    uri.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::store_context_service_server::{StoreContextService, StoreContextServiceServer};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio_stream::wrappers::TcpListenerStream;

    fn query() -> GetRenderContextRequest {
        GetRenderContextRequest {
            tenant_id: "demo-a".into(),
            storefront_id: "small".into(),
            locale: "en".into(),
            configuration: "published".into(),
            page: "collection_page_2".into(),
            current_page: 2,
            cart_id: "default".into(),
            request_id: "transport-1".into(),
        }
    }
    fn reply(query: &GetRenderContextRequest) -> GetRenderContextResponse {
        let page_type = if query.page == "index" {
            "index"
        } else if query.page == "product" {
            "product"
        } else {
            "collection"
        };
        let context = json!({"synthetic":true,"schema_version":1,"theme":{"sha":THEME_SHA,"configuration_source":"service","settings":{}},"globals":{"request":{"locale":{"iso_code":query.locale}},"localization":{"language":{"iso_code":query.locale}}},"pages":{&query.page:{"type":page_type,"template":format!("templates/{page_type}.json"),"current_page":query.current_page}},"manifest":{"service_scope":{"tenant_id":query.tenant_id,"storefront_id":query.storefront_id,"locale":query.locale,"configuration":query.configuration,"cart_id":query.cart_id,"page":query.page,"current_page":query.current_page}}});
        let context_json = serde_json::to_vec(&context).unwrap();
        GetRenderContextResponse {
            context_sha256: format!("{:x}", Sha256::digest(&context_json)),
            context_json,
            snapshot_revision: "synthetic-revision-1".into(),
            tenant_id: query.tenant_id.clone(),
            storefront_id: query.storefront_id.clone(),
            locale: query.locale.clone(),
            configuration: query.configuration.clone(),
            page: query.page.clone(),
            current_page: query.current_page,
            cart_id: query.cart_id.clone(),
            request_id: query.request_id.clone(),
        }
    }

    #[test]
    fn snapshot_rejects_hash_scope_json_and_selected_page_mismatches() {
        let query = query();
        let valid = reply(&query);
        assert!(validate_snapshot(&query, &valid).is_ok());
        for change in 0..6 {
            let mut wrong = valid.clone();
            match change {
                0 => wrong.context_sha256 = "0".repeat(64),
                1 => wrong.tenant_id = "demo-b".into(),
                2 => wrong.request_id = "different".into(),
                3 => wrong.snapshot_revision.clear(),
                4 => wrong.current_page = 1,
                _ => {
                    wrong.context_json = b"{}".to_vec();
                    wrong.context_sha256 = format!("{:x}", Sha256::digest(&wrong.context_json));
                }
            }
            assert!(validate_snapshot(&query, &wrong).is_err(), "case {change}");
        }
        let mut wrong = valid.clone();
        let mut context: Json = serde_json::from_slice(&wrong.context_json).unwrap();
        context["manifest"]["service_scope"]["tenant_id"] = json!("demo-b");
        wrong.context_json = serde_json::to_vec(&context).unwrap();
        wrong.context_sha256 = format!("{:x}", Sha256::digest(&wrong.context_json));
        assert!(validate_snapshot(&query, &wrong).is_err());
        for (path, value) in [
            ("/pages/collection_page_2/current_page", json!("2")),
            ("/pages/collection_page_2/current_page", Json::Null),
            (
                "/pages/collection_page_2/template",
                json!("templates/product.json"),
            ),
            ("/pages/collection_page_2/type", json!("product")),
            ("/theme/sha", json!("different")),
            ("/theme/configuration_source", json!("local")),
            ("/theme/settings", Json::Null),
            ("/theme/settings", json!("invalid")),
            ("/manifest/service_scope/page", json!("collection")),
            ("/manifest/service_scope/current_page", json!(1)),
        ] {
            let mut wrong = valid.clone();
            let mut context: Json = serde_json::from_slice(&wrong.context_json).unwrap();
            *context.pointer_mut(path).unwrap() = value;
            wrong.context_json = serde_json::to_vec(&context).unwrap();
            wrong.context_sha256 = format!("{:x}", Sha256::digest(&wrong.context_json));
            assert!(validate_snapshot(&query, &wrong).is_err(), "path {path}");
        }
        let mut wrong = valid.clone();
        wrong.snapshot_revision = "x".repeat(129);
        assert!(validate_snapshot(&query, &wrong).is_err());
    }

    #[test]
    fn query_bounds_and_finite_deadline_are_required() {
        assert!(validate_query(&query(), Duration::from_secs(5)).is_ok());
        assert!(validate_query(&query(), Duration::ZERO).is_err());
        assert!(validate_query(&query(), Duration::from_secs(11)).is_err());
        let mut too_big = query();
        too_big.request_id = "x".repeat(MAX_REQUEST_BYTES);
        assert!(validate_query(&too_big, Duration::from_secs(5)).is_err());
        let mut invalid = query();
        invalid.current_page = 0;
        assert!(validate_query(&invalid, Duration::from_secs(5)).is_err());
    }

    #[derive(Clone)]
    struct Authority {
        mode: u8,
        calls: Arc<AtomicUsize>,
    }
    #[tonic::async_trait]
    impl StoreContextService for Authority {
        async fn get_render_context(
            &self,
            request: tonic::Request<GetRenderContextRequest>,
        ) -> Result<tonic::Response<GetRenderContextResponse>, tonic::Status> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if request
                .metadata()
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                != Some("Bearer mock-tenant-a-token")
            {
                return Err(tonic::Status::unauthenticated("wrong tenant token"));
            }
            if self.mode == 2 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let mut response = reply(request.get_ref());
            if self.mode == 1 {
                response.context_sha256 = "0".repeat(64);
            }
            Ok(tonic::Response::new(response))
        }
    }

    async fn authority(
        mode: u8,
    ) -> (
        String,
        Arc<AtomicUsize>,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let service = Authority {
            mode,
            calls: calls.clone(),
        };
        let (stop, shutdown) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(StoreContextServiceServer::new(service))
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                    let _ = shutdown.await;
                })
                .await
                .unwrap();
        });
        (endpoint, calls, stop, task)
    }

    #[tokio::test]
    async fn native_grpc_fetches_each_request_and_preserves_authoritative_bytes() {
        let (endpoint, calls, stop, task) = authority(0).await;
        for _ in 0..2 {
            let snapshot = fetch(
                &endpoint,
                "mock-tenant-a-token",
                query(),
                Duration::from_secs(1),
            )
            .await
            .unwrap();
            assert_eq!(snapshot.context_json, reply(&query()).context_json);
            assert_eq!(snapshot.metadata["response_cache"], false);
            assert_eq!(snapshot.metadata["scope"]["request_id"], "transport-1");
        }
        assert!(
            fetch(&endpoint, "wrong-token", query(), Duration::from_secs(1))
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        stop.send(()).unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn native_grpc_rejects_corrupt_reply_and_enforces_deadline_without_fallback() {
        for mode in [1, 2] {
            let (endpoint, calls, stop, task) = authority(mode).await;
            assert!(fetch(
                &endpoint,
                "mock-tenant-a-token",
                query(),
                Duration::from_millis(30)
            )
            .await
            .is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            stop.send(()).unwrap();
            task.await.unwrap();
        }
    }
}
