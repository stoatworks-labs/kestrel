//! The HTTP API as a client sees it — the contract the Companion module is
//! written against.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use kestrel_control::{router, Shared};
use kestrel_core::{NormRect, Show};
use std::sync::Arc;
use tower::ServiceExt;

fn shared() -> Arc<Shared> {
    let mut show = Show::with_outputs(4);
    show.add_roi("Lectern", NormRect::new(0.1, 0.1, 0.25, 0.25));
    show.add_roi("Drums", NormRect::new(0.6, 0.5, 0.2, 0.2));
    Shared::new(show)
}

async fn call(s: &Arc<Shared>, method: &str, path: &str, body: &str) -> serde_json::Value {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = router(s.clone()).oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "{method} {path} answered {}",
        res.status()
    );
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn get(s: &Arc<Shared>, path: &str) -> serde_json::Value {
    call(s, "GET", path, "").await
}

#[tokio::test]
async fn health_names_the_app() {
    let s = shared();
    let v = get(&s, "/api/health").await;
    assert_eq!(v["ok"], true);
    assert_eq!(v["app"], "kestrel");
}

#[tokio::test]
async fn state_lists_every_output_even_with_nothing_routed() {
    let s = shared();
    let v = get(&s, "/api/state").await;
    assert_eq!(v["outputs"].as_array().unwrap().len(), 4);
    assert_eq!(v["rois"].as_array().unwrap().len(), 2);
    assert_eq!(v["outputs_enabled"], true);
    assert_eq!(v["output_format"]["name"], "1080p50");
    for o in v["outputs"].as_array().unwrap() {
        assert!(o["assigned"].is_null());
        assert_eq!(o["on_air"], "black");
    }
}

#[tokio::test]
async fn routing_shows_up_in_the_next_snapshot() {
    let s = shared();
    let r = call(&s, "POST", "/api/route", r#"{"output":2,"roi":1}"#).await;
    assert_eq!(r["ok"], true, "{r}");

    let v = get(&s, "/api/state").await;
    let out = &v["outputs"][1];
    assert_eq!(out["assigned"], 1);
    assert_eq!(out["assigned_name"], "Lectern");
    // No input yet, so it is routed but not on air — the distinction a control
    // surface needs to light tally honestly.
    assert_eq!(out["on_air"], "no input");
    assert_eq!(v["rois"][0]["outputs"][0], 2);
}

#[tokio::test]
async fn clearing_a_crosspoint_is_a_normal_command_not_an_error() {
    let s = shared();
    call(&s, "POST", "/api/route", r#"{"output":1,"roi":1}"#).await;
    let r = call(&s, "POST", "/api/route", r#"{"output":1,"roi":null}"#).await;
    assert_eq!(r["ok"], true, "{r}");
    let v = get(&s, "/api/state").await;
    assert!(v["outputs"][0]["assigned"].is_null());
    // And it is still there, still outputting.
    assert_eq!(v["outputs"].as_array().unwrap().len(), 4);
}

#[tokio::test]
async fn a_refusal_is_a_200_with_ok_false_and_a_real_message() {
    let s = shared();
    let r = call(&s, "POST", "/api/route", r#"{"output":1,"roi":99}"#).await;
    assert_eq!(r["ok"], false);
    let msg = r["error"].as_str().unwrap_or("");
    assert!(
        !msg.is_empty() && msg.contains("99"),
        "the client must not have to invent this: {r}"
    );
}

#[tokio::test]
async fn the_global_kill_can_be_set_or_toggled() {
    let s = shared();
    call(&s, "POST", "/api/output/enable", r#"{"enabled":false}"#).await;
    assert_eq!(get(&s, "/api/state").await["outputs_enabled"], false);

    call(&s, "POST", "/api/output/enable", r#"{"toggle":true}"#).await;
    assert_eq!(get(&s, "/api/state").await["outputs_enabled"], true);

    // An empty body is a client bug, and says so rather than silently doing
    // one of the two things.
    let r = call(&s, "POST", "/api/output/enable", "{}").await;
    assert_eq!(r["ok"], false);
}

#[tokio::test]
async fn muting_leaves_every_route_intact() {
    let s = shared();
    call(&s, "POST", "/api/route", r#"{"output":1,"roi":1}"#).await;
    call(&s, "POST", "/api/output/enable", r#"{"enabled":false}"#).await;

    let v = get(&s, "/api/state").await;
    assert_eq!(v["outputs"][0]["assigned"], 1, "the route must survive");
    for o in v["outputs"].as_array().unwrap() {
        assert_eq!(o["on_air"], "muted", "every output goes to black, not just one");
    }
}

#[tokio::test]
async fn a_region_can_be_created_edited_and_deleted() {
    let s = shared();
    let r = call(
        &s,
        "POST",
        "/api/roi",
        r#"{"name":"Guitar","rect":[0.5,0.5,0.2,0.2]}"#,
    )
    .await;
    assert_eq!(r["ok"], true);
    let id = r["id"].as_u64().unwrap();
    assert_eq!(id, 3, "ids continue past the ones already in the show");

    let r = call(
        &s,
        "POST",
        &format!("/api/roi/{id}"),
        r#"{"name":"Guitar SL"}"#,
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");
    let v = get(&s, "/api/state").await;
    assert_eq!(v["rois"][2]["name"], "Guitar SL");

    let r = call(&s, "DELETE", &format!("/api/roi/{id}"), "").await;
    assert_eq!(r["ok"], true);
    assert_eq!(get(&s, "/api/state").await["rois"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn deleting_a_routed_region_clears_the_outputs_and_leaves_them_running() {
    let s = shared();
    call(&s, "POST", "/api/route", r#"{"output":1,"roi":1}"#).await;
    call(&s, "POST", "/api/route", r#"{"output":3,"roi":1}"#).await;
    call(&s, "DELETE", "/api/roi/1", "").await;

    let v = get(&s, "/api/state").await;
    let outs = v["outputs"].as_array().unwrap();
    assert_eq!(outs.len(), 4, "outputs never disappear");
    assert!(outs.iter().all(|o| o["assigned"].is_null()));
    assert!(outs.iter().all(|o| o["on_air"] == "black"));
}

#[tokio::test]
async fn a_region_with_no_area_is_refused() {
    let s = shared();
    // There is no sensible clamp for zero area, so it is a refusal rather than
    // a region that silently becomes 1% of the frame.
    let r = call(&s, "POST", "/api/roi", r#"{"name":"Bad","rect":[0.5,0.5,0,0]}"#).await;
    assert_eq!(r["ok"], false, "{r}");
    assert!(r["error"].as_str().unwrap().contains("at least"), "{r}");
}

#[tokio::test]
async fn a_region_pushed_off_the_edge_is_clamped_back_in() {
    // The other half: position is forgiving, size is not. Nudging a region off
    // the edge from a control surface should stop it at the edge, the same way
    // dragging it does.
    let s = shared();
    let r = call(
        &s,
        "POST",
        "/api/roi",
        r#"{"name":"Edge","rect":[0.9,0.9,0.4,0.4]}"#,
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");
    let v = get(&s, "/api/state").await;
    let rect = v["rois"][2]["rect"].as_array().unwrap();
    assert!((rect[0].as_f64().unwrap() - 0.6).abs() < 1e-9, "{rect:?}");
    assert!((rect[2].as_f64().unwrap() - 0.4).abs() < 1e-9, "size must survive");
}

#[tokio::test]
async fn an_outputs_idle_fill_and_fit_can_be_set() {
    let s = shared();
    let r = call(&s, "POST", "/api/output/1", r#"{"idle":"bars","fit":"fill"}"#).await;
    assert_eq!(r["ok"], true, "{r}");
    let v = get(&s, "/api/state").await;
    assert_eq!(v["outputs"][0]["idle"], "bars");
    assert_eq!(v["outputs"][0]["fit"], "fill");
    assert_eq!(v["outputs"][0]["on_air"], "bars");
}

#[tokio::test]
async fn the_revision_moves_on_a_command_so_the_feed_knows_to_push() {
    let s = shared();
    let before = get(&s, "/api/state").await["revision"].as_u64().unwrap();
    call(&s, "POST", "/api/route", r#"{"output":1,"roi":2}"#).await;
    let after = get(&s, "/api/state").await["revision"].as_u64().unwrap();
    assert!(after > before, "{before} -> {after}");
}

#[tokio::test]
async fn scale_percentage_is_reported_for_the_badge_under_each_output() {
    let s = shared();
    call(&s, "POST", "/api/route", r#"{"output":1,"roi":1}"#).await;
    let v = get(&s, "/api/state").await;
    // A 0.25-wide region on a 1920 output from a 1920 input: 4x.
    assert!((v["outputs"][0]["scale_percent"].as_f64().unwrap() - 400.0).abs() < 1e-6);
    assert_eq!(v["outputs"][0]["quality"], "heavy");
    assert!(v["outputs"][1]["scale_percent"].is_null());
}
