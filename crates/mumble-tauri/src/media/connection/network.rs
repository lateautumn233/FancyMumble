//! Network binding policy for native WebRTC connections.

use std::sync::Arc;

use webrtc::runtime::Runtime;

#[cfg(not(target_os = "windows"))]
pub(super) fn configure(
    runtime: Arc<dyn Runtime>,
) -> Result<(Arc<dyn Runtime>, Vec<String>), String> {
    Ok((runtime, vec!["0.0.0.0:0".to_owned()]))
}

#[cfg(target_os = "windows")]
pub(super) fn configure(
    runtime: Arc<dyn Runtime>,
) -> Result<(Arc<dyn Runtime>, Vec<String>), String> {
    windows::configure(runtime)
}

#[cfg(target_os = "windows")]
mod windows {
    #![allow(
        unsafe_code,
        reason = "Windows adapter enumeration and socket interface selection"
    )]

    use std::collections::{BTreeMap, HashMap};
    use std::future::Future;
    use std::io;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, UdpSocket};
    use std::os::windows::io::AsRawSocket;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::time::Duration;

    use webrtc::runtime::{
        AsyncInterval, AsyncTcpListener, AsyncTcpStream, AsyncUdpSocket, JoinHandle, Runtime,
    };
    use windows_sys::Win32::Foundation::ERROR_BUFFER_OVERFLOW;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
        GAA_FLAG_SKIP_MULTICAST, IF_TYPE_PROP_VIRTUAL, IF_TYPE_TUNNEL, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows_sys::Win32::Networking::WinSock::{
        setsockopt, WSAGetLastError, AF_INET, IPPROTO_IP, IP_UNICAST_IF, SOCKADDR_IN, SOCKET_ERROR,
    };

    const INITIAL_ADAPTER_BUFFER_BYTES: usize = 15 * 1024;
    const MAX_ADAPTER_BUFFER_ATTEMPTS: usize = 3;

    pub(super) fn configure(
        runtime: Arc<dyn Runtime>,
    ) -> Result<(Arc<dyn Runtime>, Vec<String>), String> {
        let interfaces = non_tun_ipv4_interfaces()?;
        if interfaces.is_empty() {
            return Err("no active non-TUN IPv4 network interface is available".to_owned());
        }
        let addrs = interfaces
            .keys()
            .map(|ip| format!("{ip}:0"))
            .collect::<Vec<_>>();
        let indexes = interfaces
            .into_iter()
            .map(|(ip, index)| (IpAddr::V4(ip), index))
            .collect();
        let runtime: Arc<dyn Runtime> = Arc::new(InterfaceRuntime {
            inner: runtime,
            indexes,
        });
        Ok((runtime, addrs))
    }

    fn non_tun_ipv4_interfaces() -> Result<BTreeMap<Ipv4Addr, u32>, String> {
        let buffer = adapter_buffer()?;
        let mut interfaces = BTreeMap::new();
        let mut adapter = buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        while !adapter.is_null() {
            // SAFETY: GetAdaptersAddresses returned a linked list contained in `buffer`.
            let current = unsafe { &*adapter };
            if current.OperStatus == IfOperStatusUp {
                // SAFETY: both strings belong to the current adapter entry in `buffer`.
                let friendly = unsafe { wide_string(current.FriendlyName) };
                let description = unsafe { wide_string(current.Description) };
                if is_tun_proxy_adapter(current.IfType, &friendly, &description) {
                    tracing::debug!(
                        adapter = %friendly,
                        %description,
                        if_type = current.IfType,
                        "skipping TUN proxy network adapter for WebRTC"
                    );
                } else {
                    // SAFETY: this union field is the documented IPv4 interface index.
                    let if_index = unsafe { current.Anonymous1.Anonymous.IfIndex };
                    let mut unicast = current.FirstUnicastAddress;
                    while !unicast.is_null() {
                        // SAFETY: every unicast node belongs to the same adapter buffer.
                        let address = unsafe { &*unicast };
                        // SAFETY: the socket address is valid for the lifetime of `buffer`.
                        if let Some(ip) = unsafe { ipv4_address(&address.Address) } {
                            if is_usable(ip) {
                                tracing::debug!(
                                    adapter = %friendly,
                                    address = %ip,
                                    if_index,
                                    "using network interface for WebRTC"
                                );
                                let _ = interfaces.entry(ip).or_insert(if_index);
                            }
                        }
                        unicast = address.Next;
                    }
                }
            }
            adapter = current.Next;
        }
        Ok(interfaces)
    }

    fn adapter_buffer() -> Result<Vec<u64>, String> {
        let mut bytes = INITIAL_ADAPTER_BUFFER_BYTES;
        for _ in 0..MAX_ADAPTER_BUFFER_ATTEMPTS {
            let words = bytes.div_ceil(size_of::<u64>());
            let mut buffer = vec![0_u64; words];
            let mut buffer_bytes = u32::try_from(buffer.len() * size_of::<u64>())
                .map_err(|_| "network adapter list is too large".to_owned())?;
            // SAFETY: `buffer` is writable, suitably aligned, and its byte length is supplied.
            let result = unsafe {
                GetAdaptersAddresses(
                    u32::from(AF_INET),
                    GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER,
                    std::ptr::null(),
                    buffer.as_mut_ptr().cast(),
                    &mut buffer_bytes,
                )
            };
            if result == 0 {
                return Ok(buffer);
            }
            if result == ERROR_BUFFER_OVERFLOW {
                bytes = buffer_bytes as usize;
                continue;
            }
            return Err(format!(
                "could not enumerate Windows network adapters: {}",
                io::Error::from_raw_os_error(result as i32)
            ));
        }
        Err("Windows network adapter list kept changing during enumeration".to_owned())
    }

    fn is_tun_proxy_adapter(if_type: u32, friendly: &str, description: &str) -> bool {
        if matches!(if_type, IF_TYPE_TUNNEL | IF_TYPE_PROP_VIRTUAL) {
            return true;
        }
        let identity = format!("{friendly} {description}").to_ascii_lowercase();
        [
            "wintun",
            "wireguard",
            "tun2socks",
            "tap-windows",
            "openvpn",
            "meta tunnel",
            "mihomo",
            "clash",
            "sing-box",
            "tailscale",
            "zerotier",
        ]
        .iter()
        .any(|marker| identity.contains(marker))
    }

    fn is_usable(ip: Ipv4Addr) -> bool {
        !ip.is_unspecified()
            && !ip.is_loopback()
            && !ip.is_link_local()
            && !ip.is_multicast()
            && ip != Ipv4Addr::BROADCAST
    }

    unsafe fn ipv4_address(
        address: &windows_sys::Win32::Networking::WinSock::SOCKET_ADDRESS,
    ) -> Option<Ipv4Addr> {
        if address.lpSockaddr.is_null() || address.iSockaddrLength < size_of::<SOCKADDR_IN>() as i32
        {
            return None;
        }
        // SAFETY: the caller guarantees that `address` belongs to the adapter buffer.
        let socket = unsafe { &*address.lpSockaddr.cast::<SOCKADDR_IN>() };
        if socket.sin_family != AF_INET {
            return None;
        }
        // SAFETY: `S_un_b` is the byte representation of this IPv4 address.
        let octets = unsafe { socket.sin_addr.S_un.S_un_b };
        Some(Ipv4Addr::new(
            octets.s_b1,
            octets.s_b2,
            octets.s_b3,
            octets.s_b4,
        ))
    }

    unsafe fn wide_string(value: *const u16) -> String {
        if value.is_null() {
            return String::new();
        }
        let mut len = 0;
        // Adapter names are small; the bound prevents an invalid list from walking forever.
        while len < 32_768 {
            // SAFETY: the caller guarantees that `value` belongs to the adapter buffer.
            if unsafe { *value.add(len) } == 0 {
                break;
            }
            len += 1;
        }
        // SAFETY: the preceding loop found a bounded string within the adapter allocation.
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(value, len) })
    }

    #[derive(Debug)]
    struct InterfaceRuntime {
        inner: Arc<dyn Runtime>,
        indexes: HashMap<IpAddr, u32>,
    }

    impl Runtime for InterfaceRuntime {
        fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) -> Box<dyn JoinHandle> {
            self.inner.spawn(future)
        }

        fn spawn_reactor(
            &self,
            reactor_pool_size: usize,
            future: Pin<Box<dyn Future<Output = ()> + Send>>,
        ) -> Box<dyn JoinHandle> {
            self.inner.spawn_reactor(reactor_pool_size, future)
        }

        fn wrap_udp_socket(&self, socket: UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
            if let Ok(local) = socket.local_addr() {
                if let Some(index) = self.indexes.get(&local.ip()) {
                    pin_udp_socket(&socket, *index)?;
                }
            }
            self.inner.wrap_udp_socket(socket)
        }

        fn wrap_tcp_listener(
            &self,
            listener: TcpListener,
        ) -> io::Result<Arc<dyn AsyncTcpListener>> {
            self.inner.wrap_tcp_listener(listener)
        }

        fn connect_tcp<'a>(
            &'a self,
            remote_addr: SocketAddr,
        ) -> Pin<Box<dyn Future<Output = io::Result<Arc<dyn AsyncTcpStream>>> + Send + 'a>>
        {
            self.inner.connect_tcp(remote_addr)
        }

        fn resolve_host<'a>(
            &'a self,
            host: &'a str,
        ) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send + 'a>> {
            self.inner.resolve_host(host)
        }

        fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
            self.inner.sleep(duration)
        }

        fn interval(&self, period: Duration) -> Box<dyn AsyncInterval> {
            self.inner.interval(period)
        }

        fn block_on(&self, future: Pin<Box<dyn Future<Output = ()> + '_>>) {
            self.inner.block_on(future);
        }

        fn yield_now(&self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
            self.inner.yield_now()
        }

        fn name(&self) -> &'static str {
            "windows-non-tun"
        }
    }

    fn pin_udp_socket(socket: &UdpSocket, if_index: u32) -> io::Result<()> {
        let network_index = if_index.to_be();
        let raw_socket = usize::try_from(socket.as_raw_socket()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "socket handle does not fit a Winsock SOCKET",
            )
        })?;
        // SAFETY: this is a live Winsock handle and `network_index` is a four-byte DWORD.
        let result = unsafe {
            setsockopt(
                raw_socket,
                IPPROTO_IP,
                IP_UNICAST_IF,
                std::ptr::from_ref(&network_index).cast(),
                size_of::<u32>() as i32,
            )
        };
        if result == SOCKET_ERROR {
            // SAFETY: WSAGetLastError reads the calling thread's last Winsock error.
            return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn identifies_tun_and_proxy_adapters() {
            assert!(is_tun_proxy_adapter(IF_TYPE_PROP_VIRTUAL, "", ""));
            assert!(is_tun_proxy_adapter(IF_TYPE_TUNNEL, "", ""));
            assert!(is_tun_proxy_adapter(6, "Mihomo", "Meta Tunnel"));
            assert!(is_tun_proxy_adapter(6, "VPN", "Wintun Userspace Tunnel"));
            assert!(!is_tun_proxy_adapter(
                6,
                "Ethernet",
                "Realtek PCIe GbE Family Controller"
            ));
            assert!(!is_tun_proxy_adapter(
                6,
                "vEthernet (br0)",
                "Hyper-V Virtual Ethernet Adapter"
            ));
        }
    }
}
