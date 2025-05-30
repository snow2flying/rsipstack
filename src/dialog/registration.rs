use super::{
    authenticate::{handle_client_authenticate, Credential},
    DialogId,
};
use crate::{
    transaction::{
        endpoint::EndpointInnerRef,
        key::{TransactionKey, TransactionRole},
        make_tag,
        transaction::Transaction,
    },
    transport::SipAddr,
    Error, Result,
};
use get_if_addrs::get_if_addrs;
use rsip::{HostWithPort, Response, SipMessage, StatusCode};
use rsip_dns::trust_dns_resolver::TokioAsyncResolver;
use rsip_dns::ResolvableExt;
use std::net::IpAddr;
use tracing::info;

/// SIP Registration Client
///
/// `Registration` provides functionality for SIP user agent registration
/// with a SIP registrar server. Registration is the process by which a
/// SIP user agent informs a registrar server of its current location
/// and availability for receiving calls.
///
/// # Key Features
///
/// * **User Registration** - Registers user agent with SIP registrar
/// * **Authentication Support** - Handles digest authentication challenges
/// * **Contact Management** - Manages contact URI and expiration
/// * **DNS Resolution** - Resolves registrar server addresses
/// * **Automatic Retry** - Handles authentication challenges automatically
///
/// # Registration Process
///
/// 1. **DNS Resolution** - Resolves registrar server address
/// 2. **REGISTER Request** - Sends initial REGISTER request
/// 3. **Authentication** - Handles 401/407 challenges if needed
/// 4. **Confirmation** - Receives 200 OK with registration details
/// 5. **Refresh** - Periodically refreshes registration before expiration
///
/// # Examples
///
/// ## Basic Registration
///
/// ```rust,no_run
/// # use rsipstack::dialog::registration::Registration;
/// # use rsipstack::dialog::authenticate::Credential;
/// # use rsipstack::transaction::endpoint::Endpoint;
/// # async fn example() -> rsipstack::Result<()> {
/// # let endpoint: Endpoint = todo!();
/// let credential = Credential {
///     username: "alice".to_string(),
///     password: "secret123".to_string(),
///     realm: Some("example.com".to_string()),
/// };
///
/// let mut registration = Registration::new(endpoint.inner.clone(), Some(credential));
/// let response = registration.register(&"sip.example.com".to_string()).await?;
///
/// if response.status_code == rsip::StatusCode::OK {
///     println!("Registration successful");
///     println!("Expires in: {} seconds", registration.expires());
/// }
/// # Ok(())
/// }
/// ```
///
/// ## Registration Loop
///
/// ```rust,no_run
/// # use rsipstack::dialog::registration::Registration;
/// # use rsipstack::dialog::authenticate::Credential;
/// # use rsipstack::transaction::endpoint::Endpoint;
/// # use std::time::Duration;
/// # async fn example() -> rsipstack::Result<()> {
/// # let endpoint: Endpoint = todo!();
/// # let credential: Credential = todo!();
/// # let server = "sip.example.com".to_string();
/// let mut registration = Registration::new(endpoint.inner.clone(), Some(credential));
///
/// loop {
///     match registration.register(&server).await {
///         Ok(response) if response.status_code == rsip::StatusCode::OK => {
///             let expires = registration.expires();
///             println!("Registered for {} seconds", expires);
///             
///             // Re-register before expiration (with some margin)
///             tokio::time::sleep(Duration::from_secs((expires * 3 / 4) as u64)).await;
///         },
///         Ok(response) => {
///             eprintln!("Registration failed: {}", response.status_code);
///             tokio::time::sleep(Duration::from_secs(30)).await;
///         },
///         Err(e) => {
///             eprintln!("Registration error: {}", e);
///             tokio::time::sleep(Duration::from_secs(30)).await;
///         }
///     }
/// }
/// # Ok(())
/// # }
/// ```
///
/// # Thread Safety
///
/// Registration is not thread-safe and should be used from a single task.
/// The sequence number and state are managed internally and concurrent
/// access could lead to protocol violations.
pub struct Registration {
    pub last_seq: u32,
    pub endpoint: EndpointInnerRef,
    pub credential: Option<Credential>,
    pub contact: Option<rsip::typed::Contact>,
    pub allow: rsip::headers::Allow,
}

impl Registration {
    /// Create a new registration client
    ///
    /// Creates a new Registration instance for registering with a SIP server.
    /// The registration will use the provided endpoint for network communication
    /// and credentials for authentication if required.
    ///
    /// # Parameters
    ///
    /// * `endpoint` - Reference to the SIP endpoint for network operations
    /// * `credential` - Optional authentication credentials
    ///
    /// # Returns
    ///
    /// A new Registration instance ready to perform registration
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use rsipstack::dialog::registration::Registration;
    /// # use rsipstack::dialog::authenticate::Credential;
    /// # use rsipstack::transaction::endpoint::Endpoint;
    /// # fn example() {
    /// # let endpoint: Endpoint = todo!();
    /// // Registration without authentication
    /// let registration = Registration::new(endpoint.inner.clone(), None);
    ///
    /// // Registration with authentication
    /// let credential = Credential {
    ///     username: "alice".to_string(),
    ///     password: "secret123".to_string(),
    ///     realm: Some("example.com".to_string()),
    /// };
    /// let registration = Registration::new(endpoint.inner.clone(), Some(credential));
    /// # }
    /// ```
    pub fn new(endpoint: EndpointInnerRef, credential: Option<Credential>) -> Self {
        Self {
            last_seq: 0,
            endpoint,
            credential,
            contact: None,
            allow: Default::default(),
        }
    }

    /// Get the registration expiration time
    ///
    /// Returns the expiration time in seconds for the current registration.
    /// This value is extracted from the Contact header's expires parameter
    /// in the last successful registration response.
    ///
    /// # Returns
    ///
    /// Expiration time in seconds (default: 50 if not set)
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use rsipstack::dialog::registration::Registration;
    /// # use std::time::Duration;
    /// # async fn example() {
    /// # let registration: Registration = todo!();
    /// let expires = registration.expires();
    /// println!("Registration expires in {} seconds", expires);
    ///
    /// // Schedule re-registration before expiration
    /// let refresh_time = expires * 3 / 4; // 75% of expiration time
    /// tokio::time::sleep(Duration::from_secs(refresh_time as u64)).await;
    /// # }
    /// ```
    pub fn expires(&self) -> u32 {
        self.contact
            .as_ref()
            .and_then(|c| c.expires())
            .map(|e| e.seconds().unwrap_or(50))
            .unwrap_or(50)
    }

    /// Get the first non-loopback network interface
    ///
    /// Discovers the first available non-loopback IPv4 network interface
    /// on the system. This is used to determine the local IP address
    /// for the Contact header in registration requests.
    ///
    /// # Returns
    ///
    /// * `Ok(IpAddr)` - First non-loopback IPv4 address found
    /// * `Err(Error)` - No suitable interface found
    fn get_first_non_loopback_interface() -> Result<IpAddr> {
        get_if_addrs()?
            .iter()
            .find(|i| !i.is_loopback())
            .map(|i| match i.addr {
                get_if_addrs::IfAddr::V4(ref addr) => Ok(std::net::IpAddr::V4(addr.ip)),
                _ => Err(Error::Error("No IPv4 address found".to_string())),
            })
            .unwrap_or(Err(Error::Error("No interface found".to_string())))
    }

    /// Perform SIP registration with the server
    ///
    /// Sends a REGISTER request to the specified SIP server to register
    /// the user agent's current location. This method handles the complete
    /// registration process including DNS resolution, authentication
    /// challenges, and response processing.
    ///
    /// # Parameters
    ///
    /// * `server` - SIP server hostname or IP address (e.g., "sip.example.com")
    ///
    /// # Returns
    ///
    /// * `Ok(Response)` - Final response from the registration server
    /// * `Err(Error)` - Registration failed due to network or protocol error
    ///
    /// # Registration Flow
    ///
    /// 1. **DNS Resolution** - Resolves server address and transport
    /// 2. **Request Creation** - Creates REGISTER request with proper headers
    /// 3. **Initial Send** - Sends the registration request
    /// 4. **Authentication** - Handles 401/407 challenges if credentials provided
    /// 5. **Response Processing** - Returns final response (200 OK or error)
    ///
    /// # Response Codes
    ///
    /// * `200 OK` - Registration successful
    /// * `401 Unauthorized` - Authentication required (handled automatically)
    /// * `403 Forbidden` - Registration not allowed
    /// * `404 Not Found` - User not found
    /// * `423 Interval Too Brief` - Requested expiration too short
    ///
    /// # Examples
    ///
    /// ## Successful Registration
    ///
    /// ```rust,no_run
    /// # use rsipstack::dialog::registration::Registration;
    /// # use rsip::prelude::HeadersExt;
    /// # async fn example() -> rsipstack::Result<()> {
    /// # let mut registration: Registration = todo!();
    /// let response = registration.register(&"sip.example.com".to_string()).await?;
    ///
    /// match response.status_code {
    ///     rsip::StatusCode::OK => {
    ///         println!("Registration successful");
    ///         // Extract registration details from response
    ///         if let Ok(_contact) = response.contact_header() {
    ///             println!("Registration confirmed");
    ///         }
    ///     },
    ///     rsip::StatusCode::Forbidden => {
    ///         println!("Registration forbidden");
    ///     },
    ///     _ => {
    ///         println!("Registration failed: {}", response.status_code);
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// ## Error Handling
    ///
    /// ```rust,no_run
    /// # use rsipstack::dialog::registration::Registration;
    /// # use rsipstack::Error;
    /// # async fn example() {
    /// # let mut registration: Registration = todo!();
    /// # let server = "sip.example.com".to_string();
    /// match registration.register(&server).await {
    ///     Ok(response) => {
    ///         // Handle response based on status code
    ///     },
    ///     Err(Error::DnsResolutionError(msg)) => {
    ///         eprintln!("DNS resolution failed: {}", msg);
    ///     },
    ///     Err(Error::TransportLayerError(msg, addr)) => {
    ///         eprintln!("Network error to {}: {}", addr, msg);
    ///     },
    ///     Err(e) => {
    ///         eprintln!("Registration error: {}", e);
    ///     }
    /// }
    /// # }
    /// ```
    ///
    /// # Authentication
    ///
    /// If credentials are provided during Registration creation, this method
    /// will automatically handle authentication challenges:
    ///
    /// 1. Send initial REGISTER request
    /// 2. Receive 401/407 challenge with authentication parameters
    /// 3. Calculate authentication response using provided credentials
    /// 4. Resend REGISTER with Authorization header
    /// 5. Receive final response
    ///
    /// # Network Discovery
    ///
    /// The method automatically:
    /// * Discovers local network interface for Contact header
    /// * Resolves server address using DNS SRV/A records
    /// * Determines appropriate transport protocol (UDP/TCP/TLS)
    /// * Sets up proper Via headers for response routing
    pub async fn register(&mut self, server: &String) -> Result<Response> {
        self.last_seq += 1;

        let recipient = rsip::Uri::try_from(format!("sip:{}", server))?;

        let mut to = rsip::typed::To {
            display_name: None,
            uri: recipient.clone(),
            params: vec![],
        };

        if let Some(cred) = &self.credential {
            to.uri.auth = Some(rsip::auth::Auth {
                user: cred.username.clone(),
                password: None,
            });
        }

        let form = rsip::typed::From {
            display_name: None,
            uri: to.uri.clone(),
            params: vec![],
        }
        .with_tag(make_tag());

        let first_addr = {
            let mut addr =
                SipAddr::from(HostWithPort::from(Self::get_first_non_loopback_interface()?));
            let context = rsip_dns::Context::initialize_from(
                recipient.clone(),
                rsip_dns::AsyncTrustDnsClient::new(
                    TokioAsyncResolver::tokio(Default::default(), Default::default()).unwrap(),
                ),
                rsip_dns::SupportedTransports::any(),
            )?;

            let mut lookup = rsip_dns::Lookup::from(context);
            match lookup.resolve_next().await {
                Some(target) => {
                    addr.r#type = Some(target.transport);
                    addr
                }
                None => {
                    Err(crate::Error::DnsResolutionError(format!(
                        "DNS resolution error: {}",
                        recipient
                    )))
                }?,
            }
        };
        let contact = self
            .contact
            .clone()
            .unwrap_or_else(|| rsip::typed::Contact {
                display_name: None,
                uri: rsip::Uri {
                    auth: to.uri.auth.clone(),
                    scheme: Some(rsip::Scheme::Sip),
                    host_with_port: first_addr.clone().into(),
                    params: vec![],
                    headers: vec![],
                },
                params: vec![],
            });
        let via = self.endpoint.get_via(Some(first_addr.clone()), None)?;
        let mut request = self.endpoint.make_request(
            rsip::Method::Register,
            recipient,
            via,
            form,
            to,
            self.last_seq,
        );

        request.headers.unique_push(contact.into());
        request.headers.unique_push(self.allow.clone().into());

        let key = TransactionKey::from_request(&request, TransactionRole::Client)?;
        let mut tx = Transaction::new_client(key, request, self.endpoint.clone(), None);

        tx.send().await?;
        let mut auth_sent = false;

        while let Some(msg) = tx.receive().await {
            match msg {
                SipMessage::Response(resp) => match resp.status_code {
                    StatusCode::Trying => {
                        continue;
                    }
                    StatusCode::ProxyAuthenticationRequired | StatusCode::Unauthorized => {
                        if auth_sent {
                            info!("received {} response after auth sent", resp.status_code);
                            return Ok(resp);
                        }

                        if let Some(cred) = &self.credential {
                            self.last_seq += 1;
                            tx = handle_client_authenticate(self.last_seq, tx, resp, cred).await?;
                            tx.send().await?;
                            auth_sent = true;
                            continue;
                        } else {
                            info!("received {} response without credential", resp.status_code);
                            return Ok(resp);
                        }
                    }
                    _ => {
                        info!("registration do_request done: {:?}", resp.status_code);
                        return Ok(resp);
                    }
                },
                _ => break,
            }
        }
        return Err(crate::Error::DialogError(
            "registration transaction is already terminated".to_string(),
            DialogId::try_from(&tx.original)?,
        ));
    }
}
