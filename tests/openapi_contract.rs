use std::collections::BTreeSet;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use glim::{
    API_V1_ROUTES,
    api::{ErrorBody, ErrorEnvelope, ResolveSessionRequest},
    storage::{GitProvenance, PostFileRead, SessionRead},
};
use serde_json::{Value, json};
use tower::ServiceExt;

#[test]
fn openapi_paths_and_methods_exactly_match_current_api_routes() {
    let document: Value = serde_json::from_str(include_str!("../docs/openapi-v1.json")).unwrap();
    assert_eq!(document["openapi"], "3.1.0");
    let documented = document["paths"]
        .as_object()
        .unwrap()
        .iter()
        .flat_map(|(path, item)| {
            item.as_object()
                .unwrap()
                .keys()
                .filter(|method| ["get", "head", "post", "delete"].contains(&method.as_str()))
                .map(move |method| (method.to_uppercase(), path.clone()))
        })
        .collect::<BTreeSet<_>>();
    let implemented = API_V1_ROUTES
        .iter()
        .map(|(method, path)| ((*method).to_owned(), (*path).to_owned()))
        .collect::<BTreeSet<_>>();
    assert_eq!(documented, implemented);
}

#[test]
fn checked_cli_schema_artifacts_are_versioned_and_closed() {
    let input: Value =
        serde_json::from_str(include_str!("../docs/cli-publish-v1.schema.json")).unwrap();
    let output: Value =
        serde_json::from_str(include_str!("../docs/cli-output-v1.schema.json")).unwrap();
    assert_eq!(input["properties"]["schema_version"]["const"], 1);
    assert_eq!(input["additionalProperties"], false);
    assert_eq!(
        input["properties"]["files"]["items"]["additionalProperties"],
        false
    );
    assert_eq!(
        output["oneOf"][0]["properties"]["schema_version"]["const"],
        1
    );
    assert_eq!(
        output["oneOf"][1]["properties"]["schema_version"]["const"],
        1
    );
}

#[test]
fn representative_fixtures_match_the_rust_serde_contracts() {
    let resolve = ResolveSessionRequest {
        integration_namespace: "pi".into(),
        external_key: "session-1".into(),
        project_label: "Glim".into(),
        working_directory: "/tmp/glim".into(),
    };
    assert_eq!(resolve.external_key, "session-1");
    assert_eq!(
        serde_json::to_value(resolve).unwrap()["external_key"],
        "session-1"
    );
    for invalid in [
        json!({"integration_namespace":"pi", "external_key":"x", "project_label":"Glim"}),
        json!({"integration_namespace":"pi", "external_key":"x", "project_label":"Glim", "workingDirectory":"/tmp/glim"}),
        json!({"integration_namespace":"pi", "external_key":"x", "project_label":"Glim", "working_directory":"/tmp/glim", "unknown":true}),
    ] {
        assert!(serde_json::from_value::<ResolveSessionRequest>(invalid).is_err());
    }
    let session: SessionRead = serde_json::from_value(json!({
        "id":1, "public_id":"abc123", "integration_namespace":"pi", "external_key":"session-1",
        "project":{"id":2,"label":"Glim","working_directory":"/tmp/glim"},
        "created_at":10, "last_activity_at":20
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(session).unwrap()["project"]["label"],
        "Glim"
    );

    let git: GitProvenance = serde_json::from_value(json!({
        "root":"/work", "branch":"phase-2-cli", "commit":"0123456789abcdef0123456789abcdef01234567"
    }))
    .unwrap();
    assert_eq!(git.branch.as_deref(), Some("phase-2-cli"));
    assert!(
        serde_json::from_value::<GitProvenance>(json!({
            "root":"/work", "branch":null, "commit":null, "remote":"forbidden"
        }))
        .is_err()
    );

    let file: PostFileRead = serde_json::from_value(json!({
        "position":0, "filename":"plot.png", "caption":null, "media_type":"image/png", "renderer":"image",
        "blob":{"hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","byte_size":8}, "support_assets":[]
    })).unwrap();
    assert_eq!(serde_json::to_value(file).unwrap()["renderer"], "image");
    assert!(serde_json::from_value::<PostFileRead>(json!({
        "position":0, "filename":"x", "caption":null, "media_type":"application/octet-stream", "renderer":"other",
        "blob":{"hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","byte_size":0}, "support_assets":[]
    })).is_err());
    assert!(serde_json::from_value::<PostFileRead>(json!({
        "position":0, "filename":"x", "caption":null, "media_type":"application/octet-stream", "renderer":"download",
        "blob":{"hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","byte_size":0}, "support_assets":[], "unknown":true
    })).is_err());

    let error = ErrorEnvelope {
        error: ErrorBody {
            code: "post_not_found".into(),
            message: "Post was not found".into(),
            details: serde_json::Map::from_iter([("post_id".into(), json!(99))]),
        },
    };
    assert_eq!(error.error.code, "post_not_found");
    assert_eq!(
        serde_json::to_value(error).unwrap()["error"]["code"],
        "post_not_found"
    );
    for invalid in [
        json!({}),
        json!({"error":{"code":"x","message":"m","details":{},"unknown":true}}),
        json!({"error":{"code":"x","message":"m","details":{}},"unknown":true}),
        json!({"error":{"code":"x","message":"m"}}),
        json!({"error":{"code":"x","message":"m","details":"not-an-object"}}),
    ] {
        assert!(serde_json::from_value::<ErrorEnvelope>(invalid).is_err());
    }

    let document: Value = serde_json::from_str(include_str!("../docs/openapi-v1.json")).unwrap();
    for schema in [
        "ResolveSessionRequest",
        "Session",
        "Post",
        "PostPage",
        "GitProvenance",
        "ErrorEnvelope",
    ] {
        assert!(
            document["components"]["schemas"].get(schema).is_some(),
            "missing {schema}"
        );
    }
    let visible = &document["paths"]["/api/v1/posts/{post_id}/files/{position}/content"];
    let support =
        &document["paths"]["/api/v1/posts/{post_id}/files/{position}/support/{asset_path}"];
    for operation in [
        &visible["get"],
        &visible["head"],
        &support["get"],
        &support["head"],
    ] {
        for status in ["400", "404", "416", "500", "503"] {
            assert!(
                operation["responses"].get(status).is_some(),
                "artifact operation missing {status}"
            );
        }
        assert!(
            operation["responses"]["200"]["content"]
                .get("*/*")
                .is_some(),
            "artifact response must document dynamic content type"
        );
    }

    let heartbeat = &document["paths"]["/api/v1/sessions/{public_id}/heartbeat"]["post"];
    assert!(heartbeat.get("requestBody").is_none());
    assert!(!document.to_string().contains("occurred_at"));
}

#[tokio::test]
async fn every_documented_operation_is_recognized_by_the_actual_router() {
    let document: Value = serde_json::from_str(include_str!("../docs/openapi-v1.json")).unwrap();
    let app = glim::app();
    for (path, item) in document["paths"].as_object().unwrap() {
        let uri = path
            .replace("{public_id}", "missing")
            .replace("{project_id}", "1")
            .replace("{post_id}", "1")
            .replace("{position}", "0")
            .replace("{asset_path}", "nested/asset.png");
        for method in item
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| ["get", "head", "post", "delete"].contains(&key.as_str()))
        {
            let method_value =
                axum::http::Method::from_bytes(method.to_uppercase().as_bytes()).unwrap();
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method_value)
                        .uri(&uri)
                        .header("content-type", "application/json")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_ne!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{method} {uri}"
            );
            if response.status() == StatusCode::NOT_FOUND {
                let bytes = http_body_util::BodyExt::collect(response.into_body())
                    .await
                    .unwrap()
                    .to_bytes();
                let payload: Value = serde_json::from_slice(&bytes).unwrap();
                assert_ne!(
                    payload["error"]["code"], "api_route_not_found",
                    "{method} {uri}"
                );
            }
        }
    }

    for (method, uri, status, code) in [
        (
            "GET",
            "/api/v1/undocumented",
            StatusCode::NOT_FOUND,
            "api_route_not_found",
        ),
        (
            "PATCH",
            "/api/v1/health",
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        let payload: Value = serde_json::from_slice(
            &http_body_util::BodyExt::collect(response.into_body())
                .await
                .unwrap()
                .to_bytes(),
        )
        .unwrap();
        assert_eq!(payload["error"]["code"], code);
    }
}
