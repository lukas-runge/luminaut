use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sacn::packet::ACN_SDT_MULTICAST_PORT;
use sacn::receive::SacnReceiver;
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
    error: Option<String>,
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
        let ip: Ipv4Addr = match interface_ip.parse() {
            Ok(ip) => ip,
            Err(e) => {
                let _ = on_event.send(dmxListenerChannelType {
                    universe: 0,
                    priority: 0,
                    values: vec![],
                    error: Some(format!("Invalid interface IP: {}", e)),
                });
                return;
            }
        };

        let addr = SocketAddr::new(IpAddr::V4(ip), ACN_SDT_MULTICAST_PORT);

        let mut receiver = match SacnReceiver::with_ip(addr, None) {
            Ok(r) => r,
            Err(e) => {
                let _ = on_event.send(dmxListenerChannelType {
                    universe: 0,
                    priority: 0,
                    values: vec![],
                    error: Some(e.to_string()),
                });
                return;
            }
        };

        if let Err(e) = receiver.listen_universes(&[universe]) {
            let _ = on_event.send(dmxListenerChannelType {
                universe: 0,
                priority: 0,
                values: vec![],
                error: Some(e.to_string()),
            });
            return;
        }

        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            match receiver.recv(Some(Duration::from_millis(100))) {
                Ok(packets) => {
                    for d in &packets {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        if on_event
                            .send(dmxListenerChannelType {
                                universe: d.universe,
                                priority: d.priority,
                                values: d.values[1..].to_vec(),
                                error: None,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                Err(sacn::error::errors::SacnError::Io(ref e))
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue;
                }
                Err(e) => {
                    let _ = on_event.send(dmxListenerChannelType {
                        universe: 0,
                        priority: 0,
                        values: vec![],
                        error: Some(e.to_string()),
                    });
                    return;
                }
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
                let _ = on_event.send(dmxListenerChannelType {
                    universe: 0,
                    priority: 0,
                    values: vec![],
                    error: Some(format!("Bind {}: {}", bind_addr, e)),
                });
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
                    let _ = on_event.send(dmxListenerChannelType {
                        universe: 0,
                        priority: 0,
                        values: vec![],
                        error: Some(e.to_string()),
                    });
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
