//! Small, non-blocking plugin API over the game's captured WebSocket.
use std::cell::RefCell;
use std::sync::Arc;

use crossbeam_channel::{Receiver, TryRecvError, bounded};
use hebnix_sdk::rlapi::session::{GameSession, MATCH_HISTORY_SERVICE, shared_game_session};
use mlua::{Lua, Table, Value as LuaValue};
use serde_json::Value;

use super::lua_api::to_lua;

struct PendingHistory {
    generation: u64,
    response: Receiver<Result<Value, String>>,
}

pub fn api_table(lua: &Lua) -> mlua::Result<Table> {
    build_api(lua, shared_game_session(), hebnix_sdk::log::local_player_id)
}

fn build_api(
    lua: &Lua,
    session: Arc<GameSession>,
    player_id: fn() -> Option<String>,
) -> mlua::Result<Table> {
    let api = lua.create_table()?;
    let meta = lua.create_table()?;
    let status_session = Arc::clone(&session);
    meta.set(
        "__index",
        lua.create_function(move |_, (_, key): (Table, String)| {
            Ok(if key == "enabled" {
                LuaValue::Boolean(status_session.connected())
            } else {
                LuaValue::Nil
            })
        })?,
    )?;
    meta.set(
        "__newindex",
        lua.create_function(|_, (_, key, _): (Table, String, LuaValue)| {
            Err::<(), _>(mlua::Error::RuntimeError(format!(
                "hebnix.rlapi.{key} is read-only"
            )))
        })?,
    )?;

    // Each Lua runtime owns one request. Polling cannot enqueue duplicates,
    // and unloading the plugin drops the receiver and its eventual result.
    let pending = RefCell::new(None::<PendingHistory>);
    api.set(
        "matchhistory",
        lua.create_function(move |lua, ()| {
            let mut pending = pending.borrow_mut();
            let unavailable = || {
                (
                    LuaValue::Nil,
                    Some(
                        "Enable WebSockets or RLAPI and capture Rocket League's session first."
                            .to_string(),
                    ),
                )
            };
            if !session.connected() {
                *pending = None;
                return Ok(unavailable());
            }
            let Some(generation) = session.connection_generation() else {
                *pending = None;
                return Ok(unavailable());
            };
            if let Some(job) = pending.as_ref() {
                if job.generation != generation {
                    *pending = None;
                    return Ok((
                        LuaValue::Nil,
                        Some(
                            "Rocket League's session changed; request a fresh match history."
                                .into(),
                        ),
                    ));
                }
                let result = match job.response.try_recv() {
                    Ok(result) => result,
                    Err(TryRecvError::Empty) => {
                        return Ok((LuaValue::Nil, Some("pending".into())));
                    }
                    Err(TryRecvError::Disconnected) => {
                        Err("Match history worker disconnected.".into())
                    }
                };
                *pending = None;
                return match result {
                    Ok(value) => Ok((to_lua(lua, &value)?, None)),
                    Err(error) => Ok((LuaValue::Nil, Some(error))),
                };
            }

            let (reply, response) = bounded(1);
            let request_session = Arc::clone(&session);
            if let Err(error) = std::thread::Builder::new()
                .name("plugin-match-history".into())
                .spawn(move || {
                    let result = player_id()
                        .filter(|id| !id.trim().is_empty())
                        .ok_or_else(|| {
                            "The local PlayerID was not found in Launch.log.".to_string()
                        })
                        .and_then(|id| {
                            request_session.request_for_connection(
                                generation,
                                MATCH_HISTORY_SERVICE,
                                serde_json::json!({ "PlayerID": id }),
                            )
                        });
                    let _ = reply.send(result);
                })
            {
                return Ok((
                    LuaValue::Nil,
                    Some(format!("Could not start match history request: {error}")),
                ));
            }
            *pending = Some(PendingHistory {
                generation,
                response,
            });
            Ok((LuaValue::Nil, Some("pending".into())))
        })?,
    )?;
    api.set_metatable(Some(meta))?;
    Ok(api)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn setup(session: Arc<GameSession>, player_id: fn() -> Option<String>) -> Lua {
        let lua = Lua::new();
        lua.globals()
            .set("rlapi", build_api(&lua, session, player_id).unwrap())
            .unwrap();
        lua
    }

    fn player_id() -> Option<String> {
        Some("Steam|123|0".into())
    }

    #[test]
    fn enabled_tracks_websocket_capture_and_disable() {
        let session = Arc::new(GameSession::default());
        let lua = setup(Arc::clone(&session), player_id);
        assert!(!lua.load("return rlapi.enabled").eval::<bool>().unwrap());
        session.set_websocket_enabled(true);
        assert!(!lua.load("return rlapi.enabled").eval::<bool>().unwrap());
        let connection = session.attach();
        assert!(lua.load("return rlapi.enabled").eval::<bool>().unwrap());
        assert!(connection.current());
        assert!(lua.load("rlapi.enabled = false").exec().is_err());
        session.set_websocket_enabled(false);
        assert!(!lua.load("return rlapi.enabled").eval::<bool>().unwrap());
        session.set_enabled(true);
        assert!(lua.load("return rlapi.enabled").eval::<bool>().unwrap());
        drop(connection);
        assert!(!lua.load("return rlapi.enabled").eval::<bool>().unwrap());
        let (value, error): (LuaValue, String) =
            lua.load("return rlapi.matchhistory()").eval().unwrap();
        assert!(value.is_nil());
        assert!(error.contains("capture"));
    }

    #[test]
    fn polls_one_request_and_returns_decoded_json() {
        let session = Arc::new(GameSession::default());
        session.set_websocket_enabled(true);
        let connection = session.attach();
        let lua = setup(session, player_id);
        let (_, state): (LuaValue, String) =
            lua.load("return rlapi.matchhistory()").eval().unwrap();
        assert_eq!(state, "pending");
        let request = connection
            .requests
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert_eq!(request.service, MATCH_HISTORY_SERVICE);
        assert_eq!(
            request.body,
            serde_json::json!({ "PlayerID": "Steam|123|0" })
        );
        for _ in 0..3 {
            let (_, state): (LuaValue, String) =
                lua.load("return rlapi.matchhistory()").eval().unwrap();
            assert_eq!(state, "pending");
        }
        assert!(connection.requests.is_empty());
        request.reply.send(Ok(serde_json::json!({ "Matches": [{ "MatchGUID": "test", "ReplayUrl": "https://example.com/replay" }] }))).unwrap();
        let (history, error) = poll_result(&lua);
        assert!(error.is_none());
        lua.globals().set("history", history).unwrap();
        assert_eq!(
            lua.load("return history.Matches[1].MatchGUID")
                .eval::<String>()
                .unwrap(),
            "test"
        );
    }

    #[test]
    fn errors_and_reconnects_do_not_return_stale_history() {
        let session = Arc::new(GameSession::default());
        session.set_enabled(true);
        let connection = session.attach();
        let lua = setup(Arc::clone(&session), player_id);
        lua.load("rlapi.matchhistory()").exec().unwrap();
        let request = connection
            .requests
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        request.reply.send(Err("server error".into())).unwrap();
        let (value, error) = poll_result(&lua);
        assert!(value.is_nil());
        assert_eq!(error.as_deref(), Some("server error"));

        lua.load("rlapi.matchhistory()").exec().unwrap();
        let request = connection
            .requests
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let _new_connection = session.attach();
        request
            .reply
            .send(Ok(serde_json::json!({ "old": true })))
            .unwrap();
        let (value, error) = poll_result(&lua);
        assert!(value.is_nil());
        assert!(error.unwrap().contains("session changed"));
    }

    #[test]
    fn missing_player_id_does_not_send_a_request() {
        let session = Arc::new(GameSession::default());
        session.set_enabled(true);
        let connection = session.attach();
        let lua = setup(session, || None);
        lua.load("rlapi.matchhistory()").exec().unwrap();
        let (value, error) = poll_result(&lua);
        assert!(value.is_nil());
        assert!(error.unwrap().contains("PlayerID"));
        assert!(connection.requests.is_empty());
    }

    fn poll_result(lua: &Lua) -> (LuaValue, Option<String>) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let result: (LuaValue, Option<String>) =
                lua.load("return rlapi.matchhistory()").eval().unwrap();
            if result.1.as_deref() != Some("pending") {
                return result;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not finish"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
