//! Fresh synthetic localhost listener measurement, not an auth/callback proof.
use serde::{Deserialize, Serialize};
use windows::Win32::Networking::WinSock::{
    bind, closesocket, getsockname, listen, socket, WSACleanup, WSAGetLastError, WSAStartup,
    AF_INET, IN_ADDR, IN_ADDR_0, IPPROTO_TCP, SOCKADDR_IN, SOCKET, SOCK_STREAM, WSADATA,
};

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Observation {
    // 0 startup, 1 socket, 2 bind, 3 listen, 4 getsockname, 5 address, 6 success, 7 version.
    stage: u8,
    code: i32,
    close: Option<i32>,
    cleanup: i32,
}
impl Observation {
    pub(super) fn positive(&self) -> bool {
        self.stage == 6 && self.code == 0 && self.close == Some(0) && self.cleanup == 0
    }
    pub(super) fn measured(&self) -> bool {
        self.positive()
            || (matches!(self.stage, 2 | 3)
                && self.code != 0
                && self.close == Some(0)
                && self.cleanup == 0)
    }
    pub(super) fn report(&self) {
        super::super::diagnostic("listener-research-stage", i32::from(self.stage));
        super::super::diagnostic("listener-research-code", self.code);
        super::super::diagnostic("listener-research-close", self.close.unwrap_or(-1));
        super::super::diagnostic("listener-research-cleanup", self.cleanup);
    }
    #[cfg(test)]
    pub(super) fn positive_fixture() -> Self {
        Self {
            stage: 6,
            code: 0,
            close: Some(0),
            cleanup: 0,
        }
    }
}
struct Winsock(bool);
impl Drop for Winsock {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                WSACleanup();
            }
        }
    }
}
struct Socket(Option<SOCKET>);
impl Drop for Socket {
    fn drop(&mut self) {
        if let Some(socket) = self.0.take() {
            unsafe {
                closesocket(socket);
            }
        }
    }
}

// No remote address/input, accept/read/write, browser or auth code. The socket
// is bound only to 127.0.0.1:0, listened with backlog1, verified then closed.
// APIs: Microsoft WinSock bind/listen/getsockname/closesocket documentation.
pub(super) fn measure() -> Observation {
    let mut result = Observation {
        stage: 0,
        code: 0,
        close: None,
        cleanup: 0,
    };
    let mut data = WSADATA::default();
    result.code = unsafe { WSAStartup(0x0202, &mut data) };
    if result.code != 0 {
        return result;
    }
    let mut winsock = Winsock(true);
    if data.wVersion != 0x0202 {
        result.stage = 7;
        result.code = i32::from(data.wVersion);
        winsock.0 = false;
        result.cleanup = if unsafe { WSACleanup() } == 0 {
            0
        } else {
            unsafe { WSAGetLastError() }.0
        };
        return result;
    }
    let raw = unsafe { socket(i32::from(AF_INET.0), SOCK_STREAM, IPPROTO_TCP.0) };
    match raw {
        Err(error) => {
            result.stage = 1;
            result.code = error.code().0;
        }
        Ok(raw) => {
            let mut owned = Socket(Some(raw));
            let fixed = SOCKADDR_IN {
                sin_family: AF_INET,
                sin_port: 0,
                sin_addr: IN_ADDR {
                    S_un: IN_ADDR_0 {
                        S_addr: u32::from_ne_bytes([127, 0, 0, 1]),
                    },
                },
                ..Default::default()
            };
            (result.stage, result.code) = (|| {
                if unsafe {
                    bind(
                        raw,
                        (&raw const fixed).cast(),
                        std::mem::size_of_val(&fixed) as i32,
                    )
                } != 0
                {
                    return (2, unsafe { WSAGetLastError() }.0);
                }
                if unsafe { listen(raw, 1) } != 0 {
                    return (3, unsafe { WSAGetLastError() }.0);
                }
                let mut actual = SOCKADDR_IN::default();
                let mut length = std::mem::size_of_val(&actual) as i32;
                if unsafe { getsockname(raw, (&raw mut actual).cast(), &mut length) } != 0 {
                    return (4, unsafe { WSAGetLastError() }.0);
                }
                if length != std::mem::size_of_val(&actual) as i32
                    || actual.sin_family != AF_INET
                    || actual.sin_port == 0
                    || unsafe { actual.sin_addr.S_un.S_addr }.to_ne_bytes() != [127, 0, 0, 1]
                {
                    return (5, 0);
                }
                (6, 0)
            })();
            // Do not retry an uncertain closesocket result: numeric handles may
            // be reused. A failure rejects the experiment; process exit releases
            // any unresolved socket. Drop remains an unwind-only safety net.
            let raw = owned.0.take().expect("fresh socket guard");
            result.close = Some(if unsafe { closesocket(raw) } == 0 {
                0
            } else {
                unsafe { WSAGetLastError() }.0
            });
        }
    }
    winsock.0 = false;
    result.cleanup = if unsafe { WSACleanup() } == 0 {
        0
    } else {
        unsafe { WSAGetLastError() }.0
    };
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_complete_local_listen_or_observed_bind_listen_error_is_measurement() {
        assert!(Observation::positive_fixture().positive());
        for stage in [2, 3] {
            let value = Observation {
                stage,
                code: 10013,
                close: Some(0),
                cleanup: 0,
            };
            assert!(value.measured());
            assert!(!value.positive());
        }
        for stage in [0, 1, 4, 5, 7] {
            assert!(!Observation {
                stage,
                code: 10013,
                close: Some(0),
                cleanup: 0
            }
            .measured());
        }
        for close in [None, Some(10038)] {
            assert!(!Observation {
                close,
                ..Observation::positive_fixture()
            }
            .measured());
        }
        assert!(!Observation {
            cleanup: 10093,
            ..Observation::positive_fixture()
        }
        .measured());
    }
}
