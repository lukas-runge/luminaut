use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sacn::packet::{
    AcnRootLayerProtocol, E131RootLayerData, ACN_SDT_MULTICAST_PORT,
    E131_NETWORK_DATA_LOSS_TIMEOUT,
};
use socket2::{Domain, Protocol, Socket, Type};
use tauri::ipc::Channel;

// ── Cancellation state ────────────────────────────────────────────────────────

struct ListenerState {
    stop_flag: Mutex<Option<Arc<AtomicBool>>>,
}

impl ListenerState {
    fn new() -> Self {
        Self {
            stop_flag: Mutex::new(None),
        }
    }

    /// Cancel any running listener and return a fresh flag for the next one.
    fn arm(&self) -> Arc<AtomicBool> {
        let mut lock = self.stop_flag.lock().unwrap();
        if let Some(old) = lock.take() {
            old.store(true, Ordering::Relaxed);
        }
        let flag = Arc::new(AtomicBool::new(false));
        *lock = Some(flag.clone());
        flag
    }

    fn stop(&self) {
        let mut lock = self.stop_flag.lock().unwrap();
        if let Some(flag) = lock.take() {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

// ── Shared types ──────────────────────────────────────────────────────────────

#[derive(serde::Serialize, Clone)]
struct NetworkInterface {
    name: String,
    ip: String,
}

#[derive(serde::Serialize)]
#[allow(non_camel_case_types)]
struct dmxListenerChannelType {
    universe: u16,
    priority: u8,
    values: Vec<u8>,
    /// Every sACN sender on the universe; always empty for Art-Net.
    sources: Vec<SacnSource>,
    error: Option<String>,
}

impl dmxListenerChannelType {
    fn error(message: String) -> Self {
        Self {
            universe: 0,
            priority: 0,
            values: vec![],
            sources: vec![],
            error: Some(message),
        }
    }
}

#[derive(serde::Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
struct SacnSource {
    cid: String,
    name: String,
    ip: String,
    priority: u8,
    /// Level packets per second.
    rate: u32,
    /// Sender also transmits 0xDD per-address priority.
    per_address_priority: bool,
    /// Sender is at the winning priority and contributes to the merged output.
    winning: bool,
    values: Vec<u8>,
}

// ── sACN source tracking ──────────────────────────────────────────────────────

const START_CODE_LEVELS: u8 = 0x00;
const START_CODE_PER_ADDRESS_PRIORITY: u8 = 0xDD;

/// One received E1.31 data packet, already filtered to the monitored universe.
struct Frame<'a> {
    cid: [u8; 16],
    name: &'a str,
    ip: IpAddr,
    priority: u8,
    sequence: u8,
    start_code: u8,
    slots: &'a [u8],
}

struct SourceState {
    name: String,
    ip: IpAddr,
    priority: u8,
    /// Empty until the first NULL start code packet arrives.
    values: Vec<u8>,
    per_address_priority: bool,
    sequence: u8,
    last_seen: Instant,
    level_packets: u32,
    rate: u32,
}

/// Per-sender state for one universe. The highest priority wins; senders tied
/// at the highest priority are HTP-merged (ANSI E1.31-2018 Section 6.2.3).
#[derive(Default)]
struct Sources {
    by_cid: HashMap<[u8; 16], SourceState>,
    rate_window_start: Option<Instant>,
}

impl Sources {
    /// Returns false if the packet was discarded as out of sequence.
    fn receive(&mut self, frame: Frame, now: Instant) -> bool {
        if let Some(existing) = self.by_cid.get(&frame.cid) {
            // ANSI E1.31-2018 Section 6.7.2: drop packets 1..=19 behind the last one.
            let diff = frame.sequence.wrapping_sub(existing.sequence) as i8;
            if diff <= 0 && diff > -20 {
                return false;
            }
        }

        let source = self.by_cid.entry(frame.cid).or_insert_with(|| SourceState {
            name: String::new(),
            ip: frame.ip,
            priority: frame.priority,
            values: vec![],
            per_address_priority: false,
            sequence: frame.sequence,
            last_seen: now,
            level_packets: 0,
            rate: 0,
        });
        source.name = frame.name.to_string();
        source.ip = frame.ip;
        source.sequence = frame.sequence;
        source.last_seen = now;

        match frame.start_code {
            START_CODE_LEVELS => {
                source.priority = frame.priority;
                source.values = frame.slots.to_vec();
                source.level_packets += 1;
            }
            START_CODE_PER_ADDRESS_PRIORITY => source.per_address_priority = true,
            _ => {}
        }
        true
    }

    fn terminate(&mut self, cid: &[u8; 16]) -> bool {
        self.by_cid.remove(cid).is_some()
    }

    /// Drop senders silent for longer than the E1.31 network data loss timeout.
    fn expire(&mut self, now: Instant) -> bool {
        let before = self.by_cid.len();
        self.by_cid
            .retain(|_, s| now.duration_since(s.last_seen) <= E131_NETWORK_DATA_LOSS_TIMEOUT);
        self.by_cid.len() != before
    }

    /// Recompute packet rates once per second. Returns true when they changed.
    fn update_rates(&mut self, now: Instant) -> bool {
        let start = *self.rate_window_start.get_or_insert(now);
        let elapsed = now.duration_since(start).as_secs_f64();
        if elapsed < 1.0 {
            return false;
        }
        for s in self.by_cid.values_mut() {
            s.rate = (s.level_packets as f64 / elapsed).round() as u32;
            s.level_packets = 0;
        }
        self.rate_window_start = Some(now);
        true
    }

    fn winning_priority(&self) -> Option<u8> {
        self.with_levels().map(|s| s.priority).max()
    }

    fn with_levels(&self) -> impl Iterator<Item = &SourceState> {
        self.by_cid.values().filter(|s| !s.values.is_empty())
    }

    fn merged(&self) -> Option<(u8, Vec<u8>)> {
        let top = self.winning_priority()?;
        let mut out: Vec<u8> = Vec::new();
        for s in self.with_levels().filter(|s| s.priority == top) {
            if s.values.len() > out.len() {
                out.resize(s.values.len(), 0);
            }
            for (o, v) in out.iter_mut().zip(&s.values) {
                *o = (*o).max(*v);
            }
        }
        Some((top, out))
    }

    /// All senders, highest priority first.
    fn snapshot(&self) -> Vec<SacnSource> {
        let top = self.winning_priority();
        let mut list: Vec<SacnSource> = self
            .by_cid
            .iter()
            .map(|(cid, s)| SacnSource {
                cid: format_cid(cid),
                name: s.name.clone(),
                ip: s.ip.to_string(),
                priority: s.priority,
                rate: s.rate,
                per_address_priority: s.per_address_priority,
                winning: !s.values.is_empty() && Some(s.priority) == top,
                values: s.values.clone(),
            })
            .collect();
        list.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.cid.cmp(&b.cid))
        });
        list
    }
}

fn format_cid(cid: &[u8; 16]) -> String {
    let hex: String = cid.iter().map(|b| format!("{:02x}", b)).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Bind the sACN port and join the universe's multicast group (ANSI E1.31-2018 Section 9.3.1).
/// Unicast packets to this host arrive on the same socket.
fn open_sacn_socket(universe: u16, interface: Ipv4Addr) -> std::io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // Let other sACN tools on this machine share the port.
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), ACN_SDT_MULTICAST_PORT);
    socket.bind(&bind.into())?;
    let group = Ipv4Addr::new(239, 255, (universe >> 8) as u8, universe as u8);
    socket.join_multicast_v4(&group, &interface)?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    Ok(socket.into())
}

// ── Commands ──────────────────────────────────────────────────────────────────

#[tauri::command]
fn get_network_interfaces() -> Vec<NetworkInterface> {
    let mut result = vec![NetworkInterface {
        name: "Any".to_string(),
        ip: "0.0.0.0".to_string(),
    }];

    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            match iface.addr {
                if_addrs::IfAddr::V4(ref v4) if !v4.ip.is_loopback() => {
                    result.push(NetworkInterface {
                        name: format!("{} ({})", iface.name, v4.ip),
                        ip: v4.ip.to_string(),
                    });
                }
                _ => {}
            }
        }
    }

    result
}

#[tauri::command]
fn stop_listener(state: tauri::State<ListenerState>) {
    state.stop();
}

#[tauri::command]
#[allow(non_snake_case)]
fn sAcnListener(
    universe: u16,
    interface_ip: String,
    on_event: Channel<dmxListenerChannelType>,
    state: tauri::State<ListenerState>,
) {
    let stop = state.arm();

    std::thread::spawn(move || {
        let interface: Ipv4Addr = match interface_ip.parse() {
            Ok(ip) => ip,
            Err(e) => {
                let _ = on_event.send(dmxListenerChannelType::error(format!(
                    "Invalid interface IP: {}",
                    e
                )));
                return;
            }
        };

        let socket = match open_sacn_socket(universe, interface) {
            Ok(s) => s,
            Err(e) => {
                let _ = on_event.send(dmxListenerChannelType::error(format!(
                    "Listen on universe {}: {}",
                    universe, e
                )));
                return;
            }
        };

        // Cap UI updates; senders can push 44 fps each.
        const EMIT_INTERVAL: Duration = Duration::from_millis(33);

        let mut sources = Sources::default();
        let mut buffer = [0u8; 1144];
        let mut dirty = false;
        let mut last_emit: Option<Instant> = None;

        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            match socket.recv_from(&mut buffer) {
                Ok((length, from)) => {
                    let Ok(packet) = AcnRootLayerProtocol::parse(&buffer[..length]) else {
                        continue;
                    };
                    let cid = *packet.pdu.cid.as_bytes();
                    if let E131RootLayerData::DataPacket(d) = packet.pdu.data {
                        if d.universe != universe || d.preview_data {
                            continue;
                        }
                        let values = &d.data.property_values;
                        let Some((&start_code, slots)) = values.split_first() else {
                            continue;
                        };
                        dirty |= if d.stream_terminated {
                            sources.terminate(&cid)
                        } else {
                            sources.receive(
                                Frame {
                                    cid,
                                    name: &d.source_name,
                                    ip: from.ip(),
                                    priority: d.priority,
                                    sequence: d.sequence_number,
                                    start_code,
                                    slots,
                                },
                                Instant::now(),
                            )
                        };
                    }
                }
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => {
                    let _ = on_event.send(dmxListenerChannelType::error(e.to_string()));
                    return;
                }
            }

            let now = Instant::now();
            dirty |= sources.expire(now);
            dirty |= sources.update_rates(now);
            if !dirty || last_emit.is_some_and(|t| now.duration_since(t) < EMIT_INTERVAL) {
                continue;
            }
            dirty = false;
            last_emit = Some(now);

            if stop.load(Ordering::Relaxed) {
                return;
            }
            let (priority, values) = sources.merged().unwrap_or_default();
            if on_event
                .send(dmxListenerChannelType {
                    universe,
                    priority,
                    values,
                    sources: sources.snapshot(),
                    error: None,
                })
                .is_err()
            {
                return;
            }
        }
    });
}

/// Art-Net port address = (net & 0x7F) << 8 | (subnet & 0x0F) << 4 | (universe & 0x0F)
#[tauri::command]
#[allow(non_snake_case)]
fn artNetListener(
    net: u8,
    subnet: u8,
    universe: u8,
    interface_ip: String,
    on_event: Channel<dmxListenerChannelType>,
    state: tauri::State<ListenerState>,
) {
    use artnet_protocol::ArtCommand;
    use std::net::UdpSocket;

    let port_address: u16 =
        ((net as u16 & 0x7F) << 8) | ((subnet as u16 & 0x0F) << 4) | (universe as u16 & 0x0F);

    let stop = state.arm();

    std::thread::spawn(move || {
        let bind_addr = format!("{}:6454", interface_ip);
        let socket = match UdpSocket::bind(&bind_addr) {
            Ok(s) => s,
            Err(e) => {
                let _ = on_event.send(dmxListenerChannelType::error(format!(
                    "Bind {}: {}",
                    bind_addr, e
                )));
                return;
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .ok();

        let mut buffer = [0u8; 1024];
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            match socket.recv_from(&mut buffer) {
                Ok((length, _addr)) => {
                    let command = match ArtCommand::from_buffer(&buffer[..length]) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    if let ArtCommand::Output(output) = command {
                        let recv_port = u16::from(output.port_address);
                        if recv_port != port_address {
                            continue;
                        }
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        if on_event
                            .send(dmxListenerChannelType {
                                universe: recv_port,
                                priority: 0,
                                values: output.data.as_ref().clone(),
                                sources: vec![],
                                error: None,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue;
                }
                Err(e) => {
                    let _ = on_event.send(dmxListenerChannelType::error(e.to_string()));
                    return;
                }
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(ListenerState::new())
        .invoke_handler(tauri::generate_handler![
            stop_listener,
            get_network_interfaces,
            sAcnListener,
            artNetListener,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(cid: u8, priority: u8, sequence: u8, start_code: u8, slots: &[u8]) -> Frame<'_> {
        Frame {
            cid: [cid; 16],
            name: "src",
            ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            priority,
            sequence,
            start_code,
            slots,
        }
    }

    #[test]
    fn highest_priority_wins_regardless_of_arrival_order() {
        let now = Instant::now();
        let mut s = Sources::default();
        s.receive(frame(1, 100, 0, 0, &[255, 255]), now);
        s.receive(frame(2, 50, 0, 0, &[10, 10]), now);
        assert_eq!(s.merged(), Some((100, vec![255, 255])));
        s.receive(frame(2, 50, 1, 0, &[20, 20]), now);
        assert_eq!(s.merged(), Some((100, vec![255, 255])));
    }

    #[test]
    fn equal_priority_is_htp_merged() {
        let now = Instant::now();
        let mut s = Sources::default();
        s.receive(frame(1, 100, 0, 0, &[255, 0, 7]), now);
        s.receive(frame(2, 100, 0, 0, &[0, 128]), now);
        assert_eq!(s.merged(), Some((100, vec![255, 128, 7])));
    }

    #[test]
    fn lower_priority_takes_over_when_higher_times_out() {
        let t0 = Instant::now();
        let mut s = Sources::default();
        s.receive(frame(1, 100, 0, 0, &[255]), t0);
        let t1 = t0 + Duration::from_secs(2);
        s.receive(frame(2, 50, 0, 0, &[10]), t1);
        assert!(s.expire(t1 + Duration::from_secs(1)));
        assert_eq!(s.merged(), Some((50, vec![10])));
    }

    #[test]
    fn terminated_source_is_removed() {
        let now = Instant::now();
        let mut s = Sources::default();
        s.receive(frame(1, 100, 0, 0, &[255]), now);
        s.receive(frame(2, 50, 0, 0, &[10]), now);
        assert!(s.terminate(&[1; 16]));
        assert_eq!(s.merged(), Some((50, vec![10])));
    }

    #[test]
    fn per_address_priority_is_flagged_not_shown_as_levels() {
        let now = Instant::now();
        let mut s = Sources::default();
        s.receive(frame(1, 100, 0, 0, &[42]), now);
        s.receive(frame(1, 100, 1, START_CODE_PER_ADDRESS_PRIORITY, &[200]), now);
        assert_eq!(s.merged(), Some((100, vec![42])));
        assert!(s.snapshot()[0].per_address_priority);
    }

    #[test]
    fn source_with_only_per_address_priority_does_not_blank_output() {
        let now = Instant::now();
        let mut s = Sources::default();
        s.receive(frame(1, 50, 0, 0, &[42]), now);
        s.receive(frame(2, 200, 0, START_CODE_PER_ADDRESS_PRIORITY, &[200]), now);
        assert_eq!(s.merged(), Some((50, vec![42])));
    }

    #[test]
    fn out_of_sequence_packet_is_dropped() {
        let now = Instant::now();
        let mut s = Sources::default();
        assert!(s.receive(frame(1, 100, 10, 0, &[1]), now));
        assert!(!s.receive(frame(1, 100, 5, 0, &[2]), now));
        assert_eq!(s.merged(), Some((100, vec![1])));
        // Sequence wraps 255 -> 0 and big jumps (sender restart) are accepted.
        assert!(s.receive(frame(1, 100, 200, 0, &[3]), now));
        assert!(s.receive(frame(1, 100, 255, 0, &[4]), now));
        assert!(s.receive(frame(1, 100, 0, 0, &[5]), now));
    }

    #[test]
    fn snapshot_lists_all_senders_and_marks_winners() {
        let now = Instant::now();
        let mut s = Sources::default();
        s.receive(frame(1, 50, 0, 0, &[1]), now);
        s.receive(frame(2, 100, 0, 0, &[2]), now);
        let list = s.snapshot();
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].priority, list[0].winning), (100, true));
        assert_eq!((list[1].priority, list[1].winning), (50, false));
        assert_eq!(list[1].values, vec![1]);
        assert_eq!(list[0].cid, "02020202-0202-0202-0202-020202020202");
    }
}
