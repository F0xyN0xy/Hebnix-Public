//! Small, non-blocking plugin API over the game's captured WebSocket.
use std::cell::RefCell;
use std::sync::Arc;

use crossbeam_channel::{Receiver, TryRecvError, bounded};
use hebnix_sdk::rlapi::session::{GameSession, MATCH_HISTORY_SERVICE, shared_game_session};
use mlua::{Lua, Table, Value as LuaValue};
use serde_json::Value;

use super::lua_api::to_lua;

struct PendingRequest {
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

    // Each function owns one request per Lua runtime. Aliases share the same
    // pending slot, so polling cannot enqueue duplicate party mutations.
    let pending = RefCell::new(None);
    let history_session = Arc::clone(&session);
    api.set(
        "matchhistory",
        lua.create_function(move |lua, ()| {
            poll_request(
                lua,
                &history_session,
                &pending,
                MATCH_HISTORY_SERVICE,
                move || {
                    player_id()
                        .filter(|id| !id.trim().is_empty())
                        .ok_or_else(|| {
                            "The local PlayerID was not found in Launch.log.".to_string()
                        })
                        .map(|id| serde_json::json!({ "PlayerID": id }))
                },
                Ok,
            )
        })?,
    )?;

    for (name, alias, service, party_path) in [
        (
            "createparty",
            "CreateParty",
            "Party/CreateParty v1",
            Some("/Info/PartyID"),
        ),
        (
            "getparty",
            "GetParty",
            "Party/GetPlayerPartyInfo v1",
            Some("/Member/PartyID"),
        ),
        (
            "sendpartyinvite",
            "SendPartyInvite",
            "Party/SendPartyInvite v2",
            None,
        ),
        ("leaveparty", "LeaveParty", "Party/LeaveParty v1", None),
    ] {
        let session = Arc::clone(&session);
        let pending = RefCell::new(None);
        let function = lua.create_function(
            move |lua, (party_id, platform_id): (Option<String>, Option<String>)| {
                poll_request(
                    lua,
                    &session,
                    &pending,
                    service,
                    move || match name {
                        "createparty" => Ok(serde_json::json!({ "bForcePartyonix": true })),
                        "sendpartyinvite" => {
                            let required = |value: Option<String>, field| {
                                value.filter(|s| !s.trim().is_empty())
                                    .ok_or_else(|| format!("{field} is required."))
                            };
                            Ok(serde_json::json!({
                                "InviteeID": required(platform_id, "Platform ID (InviteeID)")?,
                                "PartyID": required(party_id, "PartyID")?,
                            }))
                        }
                        _ => Ok(serde_json::json!({})),
                    },
                    move |value| {
                        if value.get("ok").and_then(Value::as_bool) == Some(false) {
                            return Err(format!("{alias} failed: {value}"));
                        }
                        let result = value.get("result").unwrap_or(&value);
                        match party_path {
                            Some(path) => result.pointer(path)
                                .and_then(Value::as_str)
                                .filter(|id| !id.trim().is_empty())
                                .map(|id| Value::String(id.to_owned()))
                                .ok_or_else(|| format!("{alias} response contains no PartyID; the player may not be in a party.")),
                            None => Ok(Value::Bool(true)),
                        }
                    },
                )
            },
        )?;
        api.set(name, function.clone())?;
        api.set(alias, function)?;
    }
    api.set_metatable(Some(meta))?;
    Ok(api)
}

fn poll_request(
    lua: &Lua,
    session: &Arc<GameSession>,
    pending: &RefCell<Option<PendingRequest>>,
    service: &'static str,
    payload: impl FnOnce() -> Result<Value, String> + Send + 'static,
    decode: impl FnOnce(Value) -> Result<Value, String>,
) -> mlua::Result<(LuaValue, Option<String>)> {
    let mut pending = pending.borrow_mut();
    if !session.connected() {
        *pending = None;
        return Ok((
            LuaValue::Nil,
            Some("WebSocket proxy features are unavailable.".into()),
        ));
    }
    let Some(generation) = session.connection_generation() else {
        *pending = None;
        return Ok((
            LuaValue::Nil,
            Some("Rocket League's session disconnected.".into()),
        ));
    };
    if let Some(job) = pending.as_ref() {
        if job.generation != generation {
            *pending = None;
            return Ok((
                LuaValue::Nil,
                Some("Rocket League's session changed; request again on the new session.".into()),
            ));
        }
        let result = match job.response.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return Ok((LuaValue::Nil, Some("pending".into()))),
            Err(TryRecvError::Disconnected) => Err("RLAPI worker disconnected.".into()),
        };
        *pending = None;
        return match result.and_then(decode) {
            Ok(value) => Ok((to_lua(lua, &value)?, None)),
            Err(error) => Ok((LuaValue::Nil, Some(error))),
        };
    }

    let (reply, response) = bounded(1);
    let request_session = Arc::clone(session);
    if let Err(error) = std::thread::Builder::new()
        .name("plugin-rlapi".into())
        .spawn(move || {
            let result = payload()
                .and_then(|body| request_session.request_for_connection(generation, service, body));
            let _ = reply.send(result);
        })
    {
        return Ok((
            LuaValue::Nil,
            Some(format!("Could not start RLAPI request: {error}")),
        ));
    }
    *pending = Some(PendingRequest {
        generation,
        response,
    });
    Ok((LuaValue::Nil, Some("pending".into())))
}
