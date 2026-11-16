//! Opt-in loopback RSP adapter. All target operations use the shared debug lease.
use lotus_protocol::{
    debug::{BreakpointKind, DebugCommand as D},
    envelope::SessionUuid,
};
use lotus_session::supervisor::Supervisor;
use serde_json::Value;
use std::{collections::BTreeMap, io, net::SocketAddr, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const MAX_PACKET: usize = 16_384;

pub struct Server {
    pub address: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    pub async fn start(host: Arc<Supervisor>, session: String, port: u16) -> io::Result<Self> {
        host.require_control()?;
        host.summary(&session)?;
        // Every connection starts with HIO forwarding, stepping and breakpoints, which only
        // CITnLO implements. AZAHAR answers "operation requires CITnLO experimental backend";
        // refuse here instead of accepting a connection that closes on its first packet.
        if let Err(error) = host
            .debug(
                &session,
                &D::HioForward { enabled: false },
                lotus_session::telemetry::Actor::Host,
            )
            .await
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("GDB remote requires the CITnLO backend: {error}"),
            ));
        }
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            loop {
                if host.summary(&session).is_err() {
                    break;
                }
                let accepted = tokio::select! {
                    accepted = listener.accept() => accepted,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => continue,
                };
                let Ok((socket, _)) = accepted else {
                    break;
                };
                let owner = format!("gdb:{}", SessionUuid::generate());
                if host
                    .debug_as(
                        &session,
                        &D::LeaseAcquire {
                            owner: owner.clone(),
                        },
                        &owner,
                    )
                    .await
                    .is_err()
                {
                    drop(socket);
                    continue;
                }
                let mut connection = Connection {
                    socket,
                    host: host.clone(),
                    session: session.clone(),
                    owner,
                    no_ack: false,
                    thread: None,
                    core: 0,
                    breakpoints: BTreeMap::new(),
                    last_stop: "S05".into(),
                    cleaned: false,
                };
                let watcher_host = host.clone();
                let watcher_session = session.clone();
                tokio::select! {
                    _ = connection.serve() => {},
                    _ = async {
                        loop {
                            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                            if watcher_host.summary(&watcher_session).is_err() { break; }
                        }
                    } => {},
                }
                connection.cleanup(false).await;
                // Drop performs cleanup even if the server task was cancelled.
            }
        });
        Ok(Self { address, task })
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Connection {
    socket: TcpStream,
    host: Arc<Supervisor>,
    session: String,
    owner: String,
    no_ack: bool,
    thread: Option<u32>,
    core: u32,
    breakpoints: BTreeMap<(u32, u32, u32), u64>,
    last_stop: String,
    cleaned: bool,
}
impl Drop for Connection {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        let host = self.host.clone();
        let session = self.session.clone();
        let owner = self.owner.clone();
        let ids: Vec<_> = self.breakpoints.values().copied().collect();
        tokio::spawn(async move {
            let _ = host.debug_as(&session, &D::Pause, &owner).await;
            for id in ids {
                let _ = host
                    .debug_as(&session, &D::BreakpointRemove { id }, &owner)
                    .await;
            }
            let _ = host
                .debug_as(&session, &D::HioForward { enabled: false }, &owner)
                .await;
            let _ = host
                .debug_as(
                    &session,
                    &D::LeaseRelease {
                        owner: owner.clone(),
                    },
                    &owner,
                )
                .await;
        });
    }
}
impl Connection {
    async fn cleanup(&mut self, resume: bool) {
        if self.cleaned {
            return;
        }
        let _ = self.debug(D::Pause).await;
        let ids: Vec<_> = self.breakpoints.values().copied().collect();
        for id in ids {
            let _ = self.debug(D::BreakpointRemove { id }).await;
        }
        self.breakpoints.clear();
        let _ = self.debug(D::HioForward { enabled: false }).await;
        if resume {
            let _ = self.debug(D::Continue).await;
        }
        let _ = self
            .debug(D::LeaseRelease {
                owner: self.owner.clone(),
            })
            .await;
        self.cleaned = true;
    }
    async fn debug(&self, command: D) -> io::Result<Value> {
        self.host
            .debug_as(&self.session, &command, &self.owner)
            .await
    }
    async fn serve(&mut self) -> io::Result<()> {
        self.debug(D::Pause).await?;
        self.debug(D::HioForward { enabled: true }).await?;
        loop {
            let packet = read_packet(&mut self.socket, self.no_ack).await?;
            let detach = packet == b"D" || packet.starts_with(b"D;");
            let kill = packet == b"k" || packet.starts_with(b"vKill;");
            let reply = self
                .handle(&packet)
                .await
                .unwrap_or_else(|_| b"E01".to_vec());
            if kill {
                let _ = self
                    .host
                    .stop(&self.session, lotus_session::telemetry::Actor::Host)
                    .await;
                if packet.starts_with(b"vKill;") {
                    write_packet(&mut self.socket, &reply).await?;
                }
                return Ok(());
            }
            if detach {
                self.cleanup(true).await;
            }
            write_packet(&mut self.socket, &reply).await?;
            if packet == b"QStartNoAckMode" {
                self.no_ack = true;
            }
            if detach {
                return Ok(());
            }
        }
    }
    async fn handle(&mut self, packet: &[u8]) -> io::Result<Vec<u8>> {
        // Binary X packets must be handled before interpreting UTF-8.
        if packet.first() == Some(&b'X') {
            let split = packet.iter().position(|b| *b == b':').ok_or_else(bad)?;
            let (address, length) = pair(
                std::str::from_utf8(&packet[1..split]).map_err(|_| bad())?,
                ',',
            )?;
            if packet.len() - split - 1 != length as usize {
                return Err(bad());
            }
            if length == 0 {
                return Ok(b"OK".to_vec());
            }
            self.debug(D::WriteMemory {
                address,
                bytes: packet[split + 1..].to_vec(),
            })
            .await?;
            return Ok(b"OK".to_vec());
        }
        let p = std::str::from_utf8(packet).map_err(|_| bad())?;
        let out = if p.starts_with("qSupported") {
            "PacketSize=4000;QStartNoAckMode+;qXfer:features:read+;qXfer:threads:read+;qXfer:osdata:read+;vContSupported+;swbreak+;hwbreak+;ReverseStep+;ReverseContinue+".into()
        } else if p == "QStartNoAckMode" || p == "!" {
            "OK".into()
        } else if p == "qAttached" {
            "1".into()
        } else if p == "qC" {
            let r = self.debug(D::Registers { core: self.core }).await?;
            let mut id = self
                .thread
                .unwrap_or(r["thread"].as_u64().unwrap_or(0) as u32);
            if id == 0 {
                let threads = self.debug(D::Threads).await?;
                id = threads["threads"]
                    .as_array()
                    .and_then(|ts| ts.first())
                    .and_then(|t| t["id"].as_u64())
                    .ok_or_else(bad)? as u32;
                self.thread = Some(id);
            }
            format!("QC{id:x}")
        } else if p == "?" {
            self.last_stop.clone()
        } else if p == "\u{3}" {
            self.debug(D::Pause).await?;
            self.last_stop = "S02".into();
            self.last_stop.clone()
        } else if p == "qfThreadInfo" {
            let r = self.debug(D::Threads).await?;
            format!(
                "m{}",
                r["threads"]
                    .as_array()
                    .ok_or_else(bad)?
                    .iter()
                    .filter_map(|t| t["id"].as_u64())
                    .map(|id| format!("{id:x}"))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        } else if p == "qsThreadInfo" {
            "l".into()
        } else if p.starts_with('H') || p.starts_with('T') {
            let text = if p.starts_with('H') {
                p.get(2..).ok_or_else(bad)?
            } else {
                &p[1..]
            };
            if text == "-1" || text == "0" {
                self.thread = None;
            } else {
                let id = number(text)?;
                let r = self.debug(D::Threads).await?;
                let thread = r["threads"]
                    .as_array()
                    .ok_or_else(bad)?
                    .iter()
                    .find(|t| t["id"] == id)
                    .ok_or_else(bad)?;
                if p.starts_with('H') {
                    self.thread = Some(id);
                    self.core = thread["core"].as_u64().unwrap_or(0).min(3) as u32;
                }
            }
            "OK".into()
        } else if p.starts_with("qXfer:") {
            return self.transfer(p).await;
        } else if p == "g" || p.starts_with('p') {
            let r = self
                .debug(match self.thread {
                    Some(thread) => D::ThreadRegisters { thread },
                    None => D::Registers { core: self.core },
                })
                .await?;
            let registers = register_bytes(&r)?;
            if p == "g" {
                hex(&registers)
            } else {
                let (start, length) = register_range(number(&p[1..])?)?;
                hex(&registers[start..start + length])
            }
        } else if p.starts_with('G') || p.starts_with('P') {
            let mut writes = Vec::new();
            if let Some(text) = p.strip_prefix('G') {
                let bytes = unhex(text)?;
                if bytes.len() != 332 {
                    return Err(bad());
                }
                for i in 0..51 {
                    let (offset, length) = register_range(i)?;
                    writes.push((i, bytes[offset..offset + length].to_vec()));
                }
            } else {
                let (index, bytes) = p[1..].split_once('=').ok_or_else(bad)?;
                let index = number(index)?;
                let bytes = unhex(bytes)?;
                if bytes.len() != register_range(index)?.1 {
                    return Err(bad());
                }
                writes.push((index, bytes));
            }
            for (reg, bytes) in writes {
                let index = if reg < 17 {
                    reg
                } else if reg < 49 {
                    17 + (reg - 17) * 2
                } else {
                    81 + reg - 49
                };
                for (i, value) in bytes.as_chunks::<4>().0.iter().enumerate() {
                    let value = u32::from_le_bytes(*value);
                    self.debug(match self.thread {
                        Some(thread) => D::WriteThreadRegister {
                            thread,
                            register: index + i as u32,
                            value,
                        },
                        None => D::WriteRegister {
                            core: self.core,
                            register: index + i as u32,
                            value,
                        },
                    })
                    .await?;
                }
            }
            "OK".into()
        } else if let Some(text) = p.strip_prefix('m') {
            let (address, length) = pair(text, ',')?;
            let r = self.debug(D::ReadMemory { address, length }).await?;
            let bytes: Vec<u8> =
                serde_json::from_value(r["bytes"].clone()).map_err(io::Error::other)?;
            hex(&bytes)
        } else if let Some(text) = p.strip_prefix('M') {
            let (range, bytes) = text.split_once(':').ok_or_else(bad)?;
            let (address, length) = pair(range, ',')?;
            let bytes = unhex(bytes)?;
            if length as usize != bytes.len() {
                return Err(bad());
            }
            self.debug(D::WriteMemory { address, bytes }).await?;
            "OK".into()
        } else if p.starts_with('Z') || p.starts_with('z') {
            let mut parts = p[1..].split(',');
            let kind = number(parts.next().ok_or_else(bad)?)?;
            let address = number(parts.next().ok_or_else(bad)?)?;
            let length = number(parts.next().ok_or_else(bad)?)?;
            if parts.next().is_some() {
                return Err(bad());
            }
            let key = (kind, address, length);
            let bp = match kind {
                0 | 1 => BreakpointKind::Execute,
                2 => BreakpointKind::Write,
                3 => BreakpointKind::Read,
                4 => BreakpointKind::Access,
                _ => return Ok(Vec::new()),
            };
            if p.starts_with('Z') {
                if !self.breakpoints.contains_key(&key) {
                    let r = self
                        .debug(D::BreakpointSet {
                            address,
                            kind: bp,
                            length,
                        })
                        .await?;
                    self.breakpoints
                        .insert(key, r["id"].as_u64().ok_or_else(bad)?);
                }
            } else if let Some(id) = self.breakpoints.remove(&key) {
                self.debug(D::BreakpointRemove { id }).await?;
            }
            "OK".into()
        } else if let Some(encoded) = p.strip_prefix("qRcmd,") {
            let bytes = unhex(encoded)?;
            let command = std::str::from_utf8(&bytes).map_err(|_| bad())?;
            let result = match command {
                "rewind on" => {
                    self.debug(D::RewindConfigure {
                        enabled: true,
                        capacity: 64,
                        budget_mib: 128,
                        interval_frames: None,
                        compression_level: None,
                    })
                    .await?
                }
                "rewind off" => {
                    self.debug(D::RewindConfigure {
                        enabled: false,
                        capacity: 64,
                        budget_mib: 128,
                        interval_frames: None,
                        compression_level: None,
                    })
                    .await?
                }
                "rewind status" => self.debug(D::RewindStatus).await?,
                _ => return Err(bad()),
            };
            write_packet(
                &mut self.socket,
                format!("O{}", hex(format!("{result}\n").as_bytes())).as_bytes(),
            )
            .await?;
            "OK".into()
        } else if p == "bs" {
            let result = self.debug(D::ReverseStep { core: self.core }).await?;
            self.last_stop = stop_reply(&result["stop"]);
            self.last_stop.clone()
        } else if p == "bc" {
            let events = self.host.subscribe(&self.session).await?;
            let result = self.debug(D::ReverseContinue).await?;
            if result["resume_requested"] == true {
                return self.wait_for_stop(events).await;
            }
            self.last_stop = stop_reply(&result["stop"]);
            self.last_stop.clone()
        } else if p == "vCont?" {
            "vCont;c;C;s;S;t".into()
        } else if p == "s" || p.starts_with("vCont;s") || p.starts_with("vCont;S") {
            self.select_action_thread(p).await?;
            let r = self
                .debug(D::Step {
                    core: self.core,
                    count: 1,
                })
                .await?;
            self.last_stop = stop_reply(&r["stop"]);
            self.last_stop.clone()
        } else if p == "c" || p.starts_with("vCont;c") || p.starts_with("vCont;C") {
            self.select_action_thread(p).await?;
            return self.continue_until_stop().await;
        } else if p.starts_with('s') || p.starts_with('c') {
            self.debug(D::WriteRegister {
                core: self.core,
                register: 15,
                value: number(&p[1..])?,
            })
            .await?;
            if p.starts_with('s') {
                let r = self
                    .debug(D::Step {
                        core: self.core,
                        count: 1,
                    })
                    .await?;
                self.last_stop = stop_reply(&r["stop"]);
                self.last_stop.clone()
            } else {
                return self.continue_until_stop().await;
            }
        } else if p.starts_with("vCont;t") {
            self.debug(D::Pause).await?;
            self.last_stop.clone()
        } else if p == "D" || p.starts_with("D;") {
            let ids: Vec<_> = self.breakpoints.values().copied().collect();
            for id in ids {
                self.debug(D::BreakpointRemove { id }).await?;
            }
            self.breakpoints.clear();
            "OK".into()
        } else if p == "k" || p.starts_with("vKill;") {
            "OK".into()
        } else {
            String::new()
        };
        Ok(out.into_bytes())
    }
    async fn select_action_thread(&mut self, p: &str) -> io::Result<()> {
        if let Some((_, id)) = p.split_once(':') {
            let id = number(id.split(';').next().ok_or_else(bad)?)?;
            let r = self.debug(D::Threads).await?;
            let thread = r["threads"]
                .as_array()
                .ok_or_else(bad)?
                .iter()
                .find(|t| t["id"] == id)
                .ok_or_else(bad)?;
            self.core = thread["core"].as_u64().ok_or_else(bad)? as u32;
            self.thread = Some(id);
        }
        if let Some(thread) = self.thread {
            let r = self.debug(D::Registers { core: self.core }).await?;
            if (p == "s" || p.starts_with("vCont;s") || p.starts_with("vCont;S"))
                && r["thread"]
                    .as_u64()
                    .is_some_and(|id| id != 0 && id != u64::from(thread))
            {
                return Err(io::Error::other(
                    "selected thread is not scheduled on this core",
                ));
            }
        }
        Ok(())
    }
    async fn continue_until_stop(&mut self) -> io::Result<Vec<u8>> {
        let events = self.host.subscribe(&self.session).await?;
        self.debug(D::Continue).await?;
        self.wait_for_stop(events).await
    }
    async fn wait_for_stop(
        &mut self,
        mut events: tokio::sync::broadcast::Receiver<Value>,
    ) -> io::Result<Vec<u8>> {
        loop {
            tokio::select! {
                event = events.recv() => {
                    let event = match event {
                        Ok(event) => event,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let status=self.debug(D::Status).await?;
                            if status["state"]!="paused" || !status["stop"].is_object() { continue; }
                            let mut stop=status["stop"].clone();stop["kind"]="stopped".into();
                            serde_json::json!({"data":stop})
                        },
                        Err(error) => return Err(io::Error::other(error)),
                    };
                    if event["data"]["kind"] != "stopped" { continue; }
                    let stop = &event["data"];
                    if stop["reason"] == "frame" { continue; }
                    if stop["reason"] == "hio" {
                        write_packet(&mut self.socket, stop["packet"].as_str().ok_or_else(bad)?.as_bytes()).await?;
                        loop {
                            let request = read_packet(&mut self.socket, self.no_ack).await?;
                            if request.first() == Some(&b'F') {
                                let s = std::str::from_utf8(&request[1..]).map_err(|_| bad())?;
                                let mut fields = s.split(',');
                                let result = fields.next().ok_or_else(bad)?;
                                let value = if let Some(v) = result.strip_prefix('-') { -(i64::from_str_radix(v,16).map_err(|_| bad())?) } else { i64::from_str_radix(result,16).map_err(|_| bad())? };
                                let errno = fields.next().map(number).transpose()?.unwrap_or(0);
                                self.debug(D::HioReply { value, errno, interrupted: fields.next() == Some("C") }).await?;
                                break;
                            }
                            let reply = Box::pin(self.handle(&request)).await.unwrap_or_else(|_| b"E01".to_vec());
                            write_packet(&mut self.socket, &reply).await?;
                        }
                        self.debug(D::Continue).await?;
                        continue;
                    }
                    self.last_stop = stop_reply(stop);
                    return Ok(self.last_stop.as_bytes().to_vec());
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                    self.host.pump().await;
                }
                byte = self.socket.read_u8() => {
                    match byte? { 3 => { self.debug(D::Pause).await?; self.last_stop = "S02".into(); return Ok(b"S02".to_vec()); }, b'+'|b'-' => {}, _ => return Err(bad()) }
                }
            }
        }
    }
    async fn transfer(&self, p: &str) -> io::Result<Vec<u8>> {
        let fields: Vec<_> = p.split(':').collect();
        if fields.len() != 5 || fields[2] != "read" {
            return Ok(Vec::new());
        }
        let (offset, length) = pair(fields[4], ',')?;
        if length == 0 || length > 4096 {
            return Err(bad());
        }
        let document = match fields[1] {
            "features" if fields[3] == "target.xml" => target_xml(),
            "threads" => {
                let r = self.debug(D::Threads).await?;
                let mut xml = String::from("<threads>");
                for t in r["threads"].as_array().ok_or_else(bad)? {
                    xml += &format!(
                        "<thread id=\"{:x}\" core=\"{}\" name=\"{}\"/>",
                        t["id"].as_u64().ok_or_else(bad)?,
                        t["core"],
                        escape_xml(t["name"].as_str().unwrap_or(""))
                    );
                }
                xml + "</threads>"
            }
            "osdata" => {
                if fields[3].is_empty() {
                    "<osdata type=\"types\"><item><column name=\"Type\">processes</column><column name=\"Description\">Processes</column></item></osdata>".into()
                } else if fields[3] == "processes" {
                    let r = self.debug(D::Processes).await?;
                    let mut xml = String::from("<osdata type=\"processes\">");
                    for t in r["processes"].as_array().ok_or_else(bad)? {
                        xml += &format!("<item><column name=\"pid\">{}</column><column name=\"command\">{}</column></item>", t["id"], escape_xml(t["name"].as_str().unwrap_or("")));
                    }
                    xml + "</osdata>"
                } else {
                    return Ok(Vec::new());
                }
            }
            _ => return Ok(Vec::new()),
        };
        let bytes = document.as_bytes();
        let start = (offset as usize).min(bytes.len());
        let end = start.saturating_add(length as usize).min(bytes.len());
        let mut reply = vec![if end == bytes.len() { b'l' } else { b'm' }];
        reply.extend_from_slice(&bytes[start..end]);
        Ok(reply)
    }
}
fn stop_reply(stop: &Value) -> String {
    let mut packet = match stop["reason"].as_str().unwrap_or("") {
        "exception" => "T0b".into(),
        "manual" => "T02".into(),
        _ => String::from("T05"),
    };
    if let Some(thread) = stop["thread"].as_u64() {
        packet += &format!("thread:{thread:x};");
    }
    if stop["reason"] == "watchpoint" {
        let field = if stop["write"] == true {
            "watch"
        } else {
            "rwatch"
        };
        if let Some(address) = stop["address"].as_u64() {
            packet += &format!("{field}:{address:x};");
        }
    }
    packet
}
fn target_xml() -> String {
    let mut s = String::from("<?xml version=\"1.0\"?><!DOCTYPE target SYSTEM \"gdb-target.dtd\"><target><architecture>arm</architecture><feature name=\"org.gnu.gdb.arm.core\">");
    for i in 0..16 {
        s += &format!(
            "<reg name=\"{}\" bitsize=\"32\" regnum=\"{i}\" type=\"{}\"/>",
            match i {
                13 => "sp".into(),
                14 => "lr".into(),
                15 => "pc".into(),
                _ => format!("r{i}"),
            },
            if i == 15 {
                "code_ptr"
            } else if i == 13 {
                "data_ptr"
            } else {
                "uint32"
            }
        );
    }
    s += "<reg name=\"cpsr\" bitsize=\"32\" regnum=\"16\"/></feature><feature name=\"org.gnu.gdb.arm.vfp\">";
    for i in 0..32 {
        s += &format!(
            "<reg name=\"d{i}\" bitsize=\"64\" type=\"ieee_double\" regnum=\"{}\"/>",
            17 + i
        );
    }
    s + "<reg name=\"fpscr\" bitsize=\"32\" regnum=\"49\"/></feature><feature name=\"org.lotus.arm.system\"><reg name=\"fpexc\" bitsize=\"32\" regnum=\"50\"/></feature></target>"
}
fn register_bytes(r: &Value) -> io::Result<Vec<u8>> {
    let mut values = Vec::new();
    for v in r["registers"].as_array().ok_or_else(bad)? {
        values.push(v.as_u64().ok_or_else(bad)? as u32);
    }
    if values.len() != 16 {
        return Err(bad());
    }
    values.push(r["cpsr"].as_u64().ok_or_else(bad)? as u32);
    let vfp = r["vfp"].as_array().ok_or_else(bad)?;
    if vfp.len() != 64 {
        return Err(bad());
    }
    for v in vfp {
        values.push(v.as_u64().ok_or_else(bad)? as u32);
    }
    values.push(r["fpscr"].as_u64().ok_or_else(bad)? as u32);
    values.push(r["fpexc"].as_u64().ok_or_else(bad)? as u32);
    Ok(values.into_iter().flat_map(u32::to_le_bytes).collect())
}
fn register_range(index: u32) -> io::Result<(usize, usize)> {
    match index {
        0..=16 => Ok((index as usize * 4, 4)),
        17..=48 => Ok((68 + (index as usize - 17) * 8, 8)),
        49 => Ok((324, 4)),
        50 => Ok((328, 4)),
        _ => Err(bad()),
    }
}
fn number(s: &str) -> io::Result<u32> {
    u32::from_str_radix(s, 16).map_err(|_| bad())
}
fn pair(s: &str, separator: char) -> io::Result<(u32, u32)> {
    let (a, b) = s.split_once(separator).ok_or_else(bad)?;
    Ok((number(a)?, number(b)?))
}
fn bad() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid or unsupported RSP request",
    )
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(s: &str) -> io::Result<Vec<u8>> {
    if !s.is_ascii() || !s.len().is_multiple_of(2) || s.len() > 8192 {
        return Err(bad());
    }
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| {
            u8::from_str_radix(std::str::from_utf8(b).map_err(|_| bad())?, 16).map_err(|_| bad())
        })
        .collect()
}
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

async fn read_packet(socket: &mut TcpStream, no_ack: bool) -> io::Result<Vec<u8>> {
    loop {
        let start = socket.read_u8().await?;
        if start == 3 {
            return Ok(vec![3]);
        }
        if start != b'$' {
            continue;
        }
        let mut payload = Vec::new();
        let mut checksum = 0u8;
        let mut escaped = false;
        loop {
            let byte = socket.read_u8().await?;
            if byte == b'#' && !escaped {
                break;
            }
            checksum = checksum.wrapping_add(byte);
            if escaped {
                payload.push(byte ^ 0x20);
                escaped = false;
            } else if byte == b'}' {
                escaped = true;
            } else {
                payload.push(byte);
            }
            if payload.len() > MAX_PACKET {
                return Err(bad());
            }
        }
        let mut check = [0u8; 2];
        socket.read_exact(&mut check).await?;
        if unhex(std::str::from_utf8(&check).map_err(|_| bad())?)? != [checksum] {
            if !no_ack {
                socket.write_all(b"-").await?;
            }
            continue;
        }
        if !no_ack {
            socket.write_all(b"+").await?;
        }
        return Ok(payload);
    }
}
async fn write_packet(socket: &mut TcpStream, payload: &[u8]) -> io::Result<()> {
    let mut packet = vec![b'$'];
    let mut checksum = 0u8;
    for &b in payload {
        if matches!(b, b'$' | b'#' | b'}' | b'*') {
            packet.push(b'}');
            packet.push(b ^ 0x20);
            checksum = checksum.wrapping_add(b'}').wrapping_add(b ^ 0x20);
        } else {
            packet.push(b);
            checksum = checksum.wrapping_add(b);
        }
    }
    packet.extend_from_slice(format!("#{checksum:02x}").as_bytes());
    socket.write_all(&packet).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn register_layout_and_hostile_packets() {
        assert_eq!(register_range(50).unwrap(), (328, 4));
        assert!(register_range(51).is_err());
        assert!(unhex("é").is_err());
        assert!(unhex("0").is_err());
        assert!(number("100000000").is_err());
        assert_eq!(unhex(&hex(&[0, 128, 255])).unwrap(), [0, 128, 255]);
        assert!(target_xml().contains("org.gnu.gdb.arm.vfp"));
    }
    #[tokio::test]
    async fn packets_escape_binary_and_retry_bad_checksum() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let p = read_packet(&mut s, false).await.unwrap();
            assert_eq!(p, b"X:$#}*\x00");
            write_packet(&mut s, &p).await.unwrap();
        });
        let mut socket = TcpStream::connect(address).await.unwrap();
        socket.write_all(b"$bad#00").await.unwrap();
        assert_eq!(socket.read_u8().await.unwrap(), b'-');
        write_packet(&mut socket, b"X:$#}*\x00").await.unwrap();
        assert_eq!(socket.read_u8().await.unwrap(), b'+');
        assert_eq!(read_packet(&mut socket, true).await.unwrap(), b"X:$#}*\x00");
        task.await.unwrap();
    }
}
