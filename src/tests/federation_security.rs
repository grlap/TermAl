// Owns adversarial protocol coverage for the dedicated federation transport.
// Does not enable a host or launch SSH. Split from federation_link and runtime tests.

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "federation_security_allocation.rs"]
mod allocation;
#[path = "federation_security_attestation.rs"]
mod attestation;
#[path = "federation_attestation_budget.rs"]
mod attestation_budget;
#[path = "federation_security_fixture.rs"]
mod fixture;
#[path = "federation_security_liveness.rs"]
mod liveness;
#[path = "federation_security_mailbox_fixture.rs"]
mod mailbox_fixture;
#[path = "federation_security_mailbox_outcomes.rs"]
mod mailbox_outcomes;
#[path = "federation_mailbox_availability.rs"]
mod mailbox_availability;
#[path = "federation_mailbox_availability_errors.rs"]
mod mailbox_availability_errors;
#[path = "federation_mailbox_availability_reads.rs"]
mod mailbox_availability_reads;
#[path = "federation_mailbox_keyed_reads.rs"]
mod mailbox_keyed_reads;
#[path = "federation_security_receiver_hold.rs"]
mod receiver_hold;
#[path = "federation_security_pair.rs"]
mod pair;
#[path = "federation_security_processes.rs"]
mod processes;
#[path = "federation_security_reference.rs"]
mod reference;
#[path = "federation_security_reference_http.rs"]
mod reference_http;
#[path = "federation_security_relay.rs"]
mod relay;
#[path = "federation_security_requests.rs"]
mod requests;

use fixture::{Control, Satellite, established};
use pair::Pair;
use reference::*;
use reference_http::*;
