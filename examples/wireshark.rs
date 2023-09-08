//! Creates a wintun adapter, setups routes so that the adapter gets packets from the system, and
//! writes all routed packets to a pcap file for analysis in Wireshark
//! Must be run as Administrator

use packet::Builder;
use std::{
    fs::File,
    net::IpAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use subprocess::{Popen, PopenConfig, Redirection};
use winapi::{
    shared::{ipmib, winerror, ws2def, ws2ipdef},
    um::iphlpapi,
};
use wintun::get_error_message;

static RUNNING: AtomicBool = AtomicBool::new(true);

/// Converts a rust ip addr to a SOCKADDR_INET
fn _ip_addr_to_win_addr(addr: IpAddr) -> ws2ipdef::SOCKADDR_INET {
    let mut result: ws2ipdef::SOCKADDR_INET = unsafe { std::mem::zeroed() };
    match addr {
        IpAddr::V4(v4) => {
            *unsafe { result.si_family_mut() } = ws2def::AF_INET as u16;
            unsafe { result.Ipv4_mut().sin_addr = std::mem::transmute(v4.octets()) };
        }
        IpAddr::V6(v6) => {
            *unsafe { result.si_family_mut() } = ws2def::AF_INET6 as u16;
            unsafe { result.Ipv6_mut().sin6_addr = std::mem::transmute(v6.segments()) };
        }
    }

    result
}

pub enum RouteCmdKind {
    Add,
    Set,
}

pub struct RouteCmd {
    pub kind: RouteCmdKind,
    pub cmd: String,
}

impl RouteCmd {
    pub fn add(cmd: String) -> Self {
        Self {
            kind: RouteCmdKind::Add,
            cmd,
        }
    }

    pub fn set(cmd: String) -> Self {
        Self {
            kind: RouteCmdKind::Set,
            cmd,
        }
    }
}

fn main() {
    env_logger::init();

    let wintun = wintun::load_from_path("examples/wintun/bin/amd64/wintun.dll")
        .expect("Failed to load wintun dll");

    let adapter =
        match wintun::Adapter::open(&wintun, "Demo") {
            Ok(a) => {
               log::info!("Opened adapter successfully");
                a
            }
            Err(_) => {
                match wintun::Adapter::create(&wintun, "Example", "Demo", None) {
                Ok(d) => {
                    log::info!("Created adapter successfully! ");
                    d
                },
                Err(err) => panic!("Failed to open adapter and failed to create adapter. Is process running as admin? Error: {}", err),
            }
            }
        };

    let version = wintun::get_running_driver_version(&wintun).unwrap();
    log::info!("Using wintun version: {:?}", version);

    //Give wintun interface ip and gateway
    let interface_address: IpAddr = "10.8.0.2".parse().unwrap();
    let interface_gateway: IpAddr = "10.8.0.1".parse().unwrap();
    let interface_prefix_length = 24;

    let dns_server = "1.1.1.1";

    //Get the ip address of the default gateway so we can re-route all traffic to us, then the
    //gateway
    let gateway = unsafe {
        let mut row: ipmib::MIB_IPFORWARDROW = std::mem::zeroed();
        let result = iphlpapi::GetBestRoute(
            u32::from_be_bytes([1, 1, 1, 1]),
            0,
            &mut row as *mut ipmib::MIB_IPFORWARDROW,
        );
        if result != winerror::NO_ERROR {
            log::error!("Failed to get best route: {}", get_error_message(result));
            return;
        }
        log::trace!("Route: {:?}", row.dwForwardDest.to_ne_bytes());
        log::trace!("Mask: {:?}", row.dwForwardMask.to_ne_bytes());
        log::trace!("Policy: {:?}", row.dwForwardPolicy);
        log::trace!("NextHop: {:?}", row.dwForwardNextHop.to_ne_bytes());
        let gateway_bytes = row.dwForwardNextHop.to_ne_bytes();
        if gateway_bytes == [0, 0, 0, 0] {
            log::warn!("Gateway is 0.0.0.0. This may cause problems.");
            log::warn!("Usually it is something like 192.168.0.1");
            log::warn!("Is another VPN connection active?");
        }
        IpAddr::V4(gateway_bytes.into())
    };
    log::info!("Gateway is: {}", gateway);

    let wintun_adapter_index = adapter
        .get_adapter_index()
        .expect("Failed to get adapter index");
    log::info!("Index is {}", wintun_adapter_index);

    let mut routes: Vec<RouteCmd> = Vec::new();
    routes.push(RouteCmd::set(format!(
        "interface {} metric=1",
        wintun_adapter_index
    )));
    routes.push(RouteCmd::set(format!(
        "address {} static {}/{} gateway={} store=active",
        wintun_adapter_index, interface_address, interface_prefix_length, interface_gateway
    )));

    routes.push(RouteCmd::add(format!(
        "route 0.0.0.0/1 {} {} store=active",
        wintun_adapter_index, interface_gateway
    )));

    routes.push(RouteCmd::add(format!(
        "route 128.0.0.0/1 {} {} store=active",
        wintun_adapter_index, interface_gateway
    )));

    routes.push(RouteCmd::set(format!(
        "dnsservers {} static {} register=primary validate=no",
        wintun_adapter_index, dns_server
    )));

    //Execute route commands so that the system routes packets to us
    for route in &routes {
        let mut args: Vec<String> = vec![
            "netsh".to_owned(),
            "interface".to_owned(),
            "ip".to_owned(),
            match route.kind {
                RouteCmdKind::Add => "add",
                RouteCmdKind::Set => "set",
            }
            .to_owned(),
        ];
        args.extend(route.cmd.split(' ').map(|arg| arg.to_owned()));
        log::info!("Running {:?}", &args);
        let mut result = Popen::create(
            args.as_slice(),
            PopenConfig {
                stdout: Redirection::Pipe,
                stderr: Redirection::Merge,
                ..Default::default()
            },
        )
        .expect("Failed to run cmd");

        let raw_output = result
            .communicate(None)
            .expect("Failed to get output from process")
            .0
            .unwrap();

        let output = raw_output.trim();
        let status = result.wait().expect("Failed to get process exit status");
        if !status.success() || (!output.is_empty() && output != "Ok.") {
            log::error!("Running process: {:?} failed! Output: {}", args, output);
            return;
        }
    }

    let file = File::create("out.pcap").unwrap();

    let header = pcap_file::pcap::PcapHeader {
        version_major: 2,
        version_minor: 4,
        ts_correction: 0,
        ts_accuracy: 0,
        snaplen: 65535,
        datalink: pcap_file::DataLink::RAW,
        ts_resolution: pcap_file::TsResolution::NanoSecond,
        endianness: pcap_file::Endianness::Little,
    };
    let mut writer = pcap_file::pcap::PcapWriter::with_header(file, header).unwrap();
    let main_session = Arc::new(
        adapter
            .start_session(wintun::MAX_RING_CAPACITY)
            .expect("Failed to create session"),
    );

    let reader_session = main_session.clone();
    let writer_session = main_session.clone();

    let reader = std::thread::spawn(move || {
        let mut packet_count = 0;
        log::info!("Starting reader");
        while RUNNING.load(Ordering::Relaxed) {
            match reader_session.receive_blocking() {
                Ok(mut packet) => {
                    packet_count += 1;
                    let bytes = packet.bytes_mut();
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .expect("Time went backwards");
                    let packet = pcap_file::pcap::PcapPacket::new(now, bytes.len() as u32, bytes);
                    writer.write_packet(&packet).unwrap();
                }
                Err(err) => {
                    log::error!("Got error while reading: {:?}", err);
                    break;
                }
            }
        }
        packet_count
    });
    let writer = std::thread::spawn(move || {
        log::info!("Starting writer");

        let v4_dest = match interface_address {
            IpAddr::V4(v4) => v4,
            _ => panic!("Address must be ipv4"),
        };
        while RUNNING.load(Ordering::Relaxed) {
            let mut packet = writer_session.allocate_send_packet(28).unwrap();
            let buf = packet::buffer::Slice::new(packet.bytes_mut());

            //Send random ICMP request
            let _ = packet::ip::v4::Builder::with(buf)
                .unwrap()
                .id(0x2d87)
                .unwrap()
                .ttl(64)
                .unwrap()
                .source("10.6.7.8".parse().unwrap())
                .unwrap()
                .destination(v4_dest)
                .unwrap()
                .icmp()
                .unwrap()
                .echo()
                .unwrap()
                .request()
                .unwrap()
                .identifier(42)
                .unwrap()
                .sequence(2)
                .unwrap()
                .build()
                .unwrap();

            writer_session.send_packet(packet);
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });

    std::thread::sleep(std::time::Duration::from_secs(1));
    println!("Press enter to stop example");

    let mut string = String::new();
    let _ = std::io::stdin().read_line(&mut string);
    RUNNING.store(false, Ordering::Relaxed);

    log::info!("Stopping session");
    main_session.shutdown();

    let packets_captured = reader.join().unwrap();
    writer.join().unwrap();

    log::info!("Finished session successfully!");

    for route in &routes {
        match route.kind {
            RouteCmdKind::Add => {
                let mut args: Vec<String> = vec![
                    "netsh".to_owned(),
                    "interface".to_owned(),
                    "ip".to_owned(),
                    "delete".to_owned(),
                ];

                args.extend(route.cmd.split(' ').map(|arg| arg.to_owned()));
                log::info!("Running {:?}", &args);
                let mut result = Popen::create(
                    args.as_slice(),
                    PopenConfig {
                        stdout: Redirection::Pipe,
                        stderr: Redirection::Merge,
                        ..Default::default()
                    },
                )
                .expect("Failed to run cmd");

                let raw_output = result
                    .communicate(None)
                    .expect("Failed to get output from process")
                    .0
                    .unwrap();

                let output = raw_output.trim();
                let status = result.wait().expect("Failed to get process exit status");
                if !status.success() || (!output.is_empty() && output != "Ok.") {
                    log::warn!("Running process: {:?} failed! Output: {}", args, output);
                }
            }
            RouteCmdKind::Set => {}
        }
    }

    log::info!("Saved {} captured packets to out.pcap", packets_captured);
    //`main_session` and `adapter` are both dropped
}
