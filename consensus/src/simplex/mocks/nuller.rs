//! Byzantine participant that sends nullify and finalize messages for the same view.

use crate::{
    simplex::{
        encoder::{finalize_namespace, nullify_message, nullify_namespace, proposal_message},
        wire, View,
    },
    Supervisor,
};
use commonware_cryptography::{Hasher, Scheme};
use commonware_p2p::{Receiver, Recipients, Sender};
use commonware_utils::hex;
use prost::Message;
use std::marker::PhantomData;
use tracing::debug;

pub struct Config<C: Scheme, S: Supervisor<Index = View>> {
    pub crypto: C,
    pub supervisor: S,
    pub namespace: Vec<u8>,
}

pub struct Nuller<C: Scheme, H: Hasher, S: Supervisor<Index = View>> {
    crypto: C,
    supervisor: S,
    _hasher: PhantomData<H>,

    nullify_namespace: Vec<u8>,
    finalize_namespace: Vec<u8>,
}

impl<C: Scheme, H: Hasher, S: Supervisor<Index = View>> Nuller<C, H, S> {
    pub fn new(cfg: Config<C, S>) -> Self {
        Self {
            crypto: cfg.crypto,
            supervisor: cfg.supervisor,
            _hasher: PhantomData,

            nullify_namespace: nullify_namespace(&cfg.namespace),
            finalize_namespace: finalize_namespace(&cfg.namespace),
        }
    }

    pub async fn run(
        mut self,
        voter_network: (impl Sender, impl Receiver),
        _backfiller_network: (impl Sender, impl Receiver),
    ) {
        let (mut sender, mut receiver) = voter_network;
        while let Ok((s, msg)) = receiver.recv().await {
            // Parse message
            let msg = match wire::Voter::decode(msg) {
                Ok(msg) => msg,
                Err(err) => {
                    debug!(?err, sender = hex(&s), "failed to decode message");
                    continue;
                }
            };
            let payload = match msg.payload {
                Some(payload) => payload,
                None => {
                    debug!(sender = hex(&s), "message missing payload");
                    continue;
                }
            };

            // Process message
            match payload {
                wire::voter::Payload::Notarize(notarize) => {
                    // Get our index
                    let proposal = match notarize.proposal {
                        Some(proposal) => proposal,
                        None => {
                            debug!(sender = hex(&s), "notarize missing proposal");
                            continue;
                        }
                    };
                    let view = proposal.view;
                    let public_key_index = self
                        .supervisor
                        .is_participant(view, &self.crypto.public_key())
                        .unwrap();

                    // Nullify
                    let msg = nullify_message(view);
                    let n = wire::Nullify {
                        view,
                        signature: Some(wire::Signature {
                            public_key: public_key_index,
                            signature: self.crypto.sign(&self.nullify_namespace, &msg),
                        }),
                    };
                    let msg = wire::Voter {
                        payload: Some(wire::voter::Payload::Nullify(n)),
                    }
                    .encode_to_vec();
                    sender
                        .send(Recipients::All, msg.into(), true)
                        .await
                        .unwrap();

                    // Finalize digest
                    let msg = proposal_message(view, proposal.parent, &proposal.payload);
                    let f = wire::Finalize {
                        proposal: Some(proposal.clone()),
                        signature: Some(wire::Signature {
                            public_key: public_key_index,
                            signature: self.crypto.sign(&self.finalize_namespace, &msg),
                        }),
                    };
                    let msg = wire::Voter {
                        payload: Some(wire::voter::Payload::Finalize(f)),
                    }
                    .encode_to_vec();
                    sender
                        .send(Recipients::All, msg.into(), true)
                        .await
                        .unwrap();
                }
                _ => continue,
            }
        }
    }
}