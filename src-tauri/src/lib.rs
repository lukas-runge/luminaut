use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use sacn::packet::ACN_SDT_MULTICAST_PORT;
use sacn::receive::SacnReceiver;
use tauri::ipc::Channel;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn get_greeting_for(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
fn get_time(on_event: Channel<String>) {
    tauri::async_runtime::spawn(async move {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(10));
            let time = chrono::Local::now().to_rfc3339();
            on_event.send(time).unwrap();
        }
    });
}

#[derive(serde::Serialize)]
#[allow(non_camel_case_types)]
struct dmxListenerChannelType {
    universe: u16,
    priority: u8,
    values: Vec<u8>,
    error: Option<String>,
}

#[tauri::command]
#[allow(non_snake_case)]
fn sAcnListener(universes: Vec<u16>, on_event: Channel<dmxListenerChannelType>) {
    let universes = if universes.is_empty() {
        vec![1]
    } else {
        universes
    };

    std::thread::spawn(move || {
        let addr = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
            ACN_SDT_MULTICAST_PORT,
        );

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

        if let Err(e) = receiver.listen_universes(&universes) {
            let _ = on_event.send(dmxListenerChannelType {
                universe: 0,
                priority: 0,
                values: vec![],
                error: Some(e.to_string()),
            });
            return;
        }

        loop {
            match receiver.recv(Some(Duration::from_millis(500))) {
                Ok(packets) => {
                    for d in &packets {
                        if on_event
                            .send(dmxListenerChannelType {
                                universe: d.universe,
                                priority: d.priority,
                                values: d.values[1..].to_vec(), // strip DMX start code (slot 0)
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
                }
            }
        }
    });
}

#[tauri::command]
#[allow(non_snake_case)]
fn artNetListener(universes: Vec<u16>, on_event: Channel<dmxListenerChannelType>) {
    use artnet_protocol::ArtCommand;
    use std::net::UdpSocket;

    std::thread::spawn(move || {
        let socket = match UdpSocket::bind(("0.0.0.0", 6454)) {
            Ok(s) => s,
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
        socket
            .set_read_timeout(Some(Duration::from_millis(500)))
            .ok();

        let mut buffer = [0u8; 1024];
        loop {
            match socket.recv_from(&mut buffer) {
                Ok((length, _addr)) => {
                    let command = match ArtCommand::from_buffer(&buffer[..length]) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    if let ArtCommand::Output(output) = command {
                        let port_address = u16::from(output.port_address);
                        if !universes.is_empty() && !universes.contains(&port_address) {
                            continue;
                        }
                        if on_event
                            .send(dmxListenerChannelType {
                                universe: port_address,
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
                }
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_greeting_for,
            get_time,
            sAcnListener,
            artNetListener
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
