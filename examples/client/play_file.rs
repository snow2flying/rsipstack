use std::net::SocketAddr;
use std::time::Duration;

use rsipstack::transport::udp::UdpConnection;
use rsipstack::Result;
use rsipstack::{transport::SipAddr, Error};
use rtp_rs::RtpPacketBuilder;
use tokio::select;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::{stun, MediaSessionOption};

pub async fn build_rtp_conn(
    opt: &MediaSessionOption,
    ssrc: u32,
) -> Result<(UdpConnection, String)> {
    let addr = stun::get_first_non_loopback_interface()?;
    let mut conn = None;

    for p in 0..100 {
        let port = opt.rtp_start_port + p * 2;
        if let Ok(c) =
            UdpConnection::create_connection(format!("{:?}:{}", addr, port).parse()?, None, None)
                .await
        {
            conn = Some(c);
            break;
        } else {
            info!("Failed to bind RTP socket on port: {}", port);
        }
    }

    if conn.is_none() {
        return Err(Error::Error("Failed to bind RTP socket".to_string()));
    }

    let mut conn = conn.unwrap();
    if opt.external_ip.is_none() && opt.stun {
        if let Some(ref server) = opt.stun_server {
            match stun::external_by_stun(&mut conn, &server, Duration::from_secs(5)).await {
                Ok(socket) => info!("media external IP by stun: {:?}", socket),
                Err(e) => info!(
                    "Failed to get media external IP, stunserver {} : {:?}",
                    server, e
                ),
            }
        }
    }
    let codec = 0;
    let codec_name = "PCMU";
    let socketaddr: SocketAddr = conn.get_addr().addr.to_owned().try_into()?;
    let sdp = format!(
        "v=0\r\n\
        o=- 0 0 IN IP4 {}\r\n\
        s=rsipstack example\r\n\
        c=IN IP4 {}\r\n\
        t=0 0\r\n\
        m=audio {} RTP/AVP {codec}\r\n\
        a=rtpmap:{codec} {codec_name}/8000\r\n\
        a=ssrc:{ssrc}\r\n\
        a=sendrecv\r\n",
        socketaddr.ip(),
        socketaddr.ip(),
        socketaddr.port(),
    );
    info!("RTP socket: {:?} {}", conn.get_addr(), sdp);
    Ok((conn, sdp))
}

pub async fn play_echo(conn: UdpConnection, token: CancellationToken) -> Result<()> {
    select! {
        _ = token.cancelled() => {
            info!("RTP session cancelled");
        }
        _ = async {
            loop {
                let mut mbuf = vec![0; 1500];
                let (len, addr) = match conn.recv_raw(&mut mbuf).await {
                    Ok(r) => r,
                    Err(e) => {
                        info!("Failed to receive RTP: {:?}", e);
                        break;
                    }
                };
                match conn.send_raw(&mbuf[..len], &addr).await {
                    Ok(_) => {},
                    Err(e) => {
                        info!("Failed to send RTP: {:?}", e);
                        break;
                    }
                }
            }
        } => {
            info!("playback finished, hangup");
        }
    };
    Ok(())
}

pub async fn play_example_file(
    conn: UdpConnection,
    token: CancellationToken,
    ssrc: u32,
    peer_addr: String,
) -> Result<()> {
    select! {
        _ = token.cancelled() => {
            info!("RTP session cancelled");
        }
        _ = async {
            let peer_addr = SipAddr{
                addr: peer_addr.try_into().expect("peer_addr"),
                r#type: Some(rsip::transport::Transport::Udp),
            };
            let mut ts = 0;
            let sample_size = 160;
            let mut seq = 1;
            let mut ticker = tokio::time::interval(Duration::from_millis(20));

            let example_data = tokio::fs::read("./assets/example.pcmu").await.expect("read example.pcmu");

            for chunk in example_data.chunks(sample_size) {
                let result = match RtpPacketBuilder::new()
                .payload_type(0)
                .ssrc(ssrc)
                .sequence(seq.into())
                .timestamp(ts)
                .payload(&chunk)
                .build() {
                    Ok(r) => r,
                    Err(e) => {
                        info!("Failed to build RTP packet: {:?}", e);
                        break;
                    }
                };
                ts += chunk.len() as u32;
                seq += 1;
                match conn.send_raw(&result, &peer_addr).await {
                    Ok(_) => {},
                    Err(e) => {
                        info!("Failed to send RTP: {:?}", e);
                        break;
                    }
                }
                ticker.tick().await;
            }
        } => {
            info!("playback finished, hangup");
        }
    };
    Ok(())
}
