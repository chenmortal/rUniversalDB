use crate::common::{BasicIOCtx, RemoteLeaderChangedPLm};
use crate::model::common::{LeadershipId, PaxosGroupId, QueryId};
use crate::stmpaxos2pc_tm::RMPathTrait;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;

// -----------------------------------------------------------------------------------------------
//  RMServerContext
// -----------------------------------------------------------------------------------------------

pub trait RMServerContext<T: PayloadTypes> {
  fn push_plm(&mut self, plm: T::RMPLm);

  fn send_to_tm<IO: BasicIOCtx<T::NetworkMessageT>>(
    &mut self,
    io_ctx: &mut IO,
    tm: &T::TMPath,
    msg: T::TMMessage,
  );

  fn mk_node_path(&self) -> T::RMPath;

  fn is_leader(&self) -> bool;

  fn leader_map(&self) -> &BTreeMap<PaxosGroupId, LeadershipId>;
}

pub trait TMPathTrait {
  fn to_gid(&self) -> PaxosGroupId;
}

// -----------------------------------------------------------------------------------------------
//  TMServerContext
// -----------------------------------------------------------------------------------------------

pub trait TMServerContext<T: PayloadTypes> {
  fn send_to_rm<IO: BasicIOCtx<T::NetworkMessageT>>(
    &mut self,
    io_ctx: &mut IO,
    rm: &T::RMPath,
    msg: T::RMMessage,
  );

  fn mk_node_path(&self) -> T::TMPath;

  fn is_leader(&self) -> bool;
}

// -----------------------------------------------------------------------------------------------
//  Paxos2PC
// -----------------------------------------------------------------------------------------------

pub trait PayloadTypes: Clone {
  // Meta
  type RMPLm: Serialize + DeserializeOwned + Debug + Clone + PartialEq + Eq;
  type RMPath: Serialize
    + DeserializeOwned
    + Debug
    + Clone
    + PartialEq
    + Eq
    + PartialOrd
    + Ord
    + RMPathTrait;
  type TMPath: Serialize + DeserializeOwned + Debug + Clone + PartialEq + Eq + TMPathTrait;
  type RMMessage: Serialize + DeserializeOwned + Debug + Clone + PartialEq + Eq;
  type TMMessage: Serialize + DeserializeOwned + Debug + Clone + PartialEq + Eq;
  type NetworkMessageT;
  type RMContext: RMServerContext<Self>;
  type TMContext: TMServerContext<Self>;

  // RM PLm
  type RMPreparedPLm: Serialize + DeserializeOwned + Debug + Clone + PartialEq + Eq;
  type RMCommittedPLm: Serialize + DeserializeOwned + Debug + Clone + PartialEq + Eq;
  type RMAbortedPLm: Serialize + DeserializeOwned + Debug + Clone + PartialEq + Eq;

  fn rm_plm(plm: RMPLm<Self>) -> Self::RMPLm;

  // TM-to-RM Messages
  type Prepare: Serialize + DeserializeOwned + Debug + Clone + PartialEq + Eq;

  fn rm_msg(msg: RMMessage<Self>) -> Self::RMMessage;

  // RM-to-TM Messages
  fn tm_msg(msg: TMMessage<Self>) -> Self::TMMessage;
}

// RM PLm
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RMPreparedPLm<T: PayloadTypes> {
  pub query_id: QueryId,
  pub tm: T::TMPath,
  pub rms: Vec<T::RMPath>,
  pub payload: T::RMPreparedPLm,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RMCommittedPLm<T: PayloadTypes> {
  pub query_id: QueryId,
  pub payload: T::RMCommittedPLm,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RMAbortedPLm<T: PayloadTypes> {
  pub query_id: QueryId,
  pub payload: T::RMAbortedPLm,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum RMPLm<T: PayloadTypes> {
  Prepared(RMPreparedPLm<T>),
  Committed(RMCommittedPLm<T>),
  Aborted(RMAbortedPLm<T>),
}

// TM-to-RM Messages
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Prepare<T: PayloadTypes> {
  pub query_id: QueryId,
  pub tm: T::TMPath,
  pub rms: Vec<T::RMPath>,
  /// This typically contains references to resources that this Paxos2PC
  /// should take over in the RM.
  pub payload: T::Prepare,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CheckPrepared<T: PayloadTypes> {
  pub query_id: QueryId,
  pub tm: T::TMPath,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Abort<T: PayloadTypes> {
  pub query_id: QueryId,
  pub tm: T::TMPath,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Commit<T: PayloadTypes> {
  pub query_id: QueryId,
  pub tm: T::TMPath,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum RMMessage<T: PayloadTypes> {
  Prepare(Prepare<T>),
  CheckPrepared(CheckPrepared<T>),
  Abort(Abort<T>),
  Commit(Commit<T>),
}

// RM-to-TM Messages
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Prepared<T: PayloadTypes> {
  pub query_id: QueryId,
  pub rm: T::RMPath,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct InformPrepared<T: PayloadTypes> {
  pub query_id: QueryId,
  pub tm: T::TMPath,
  pub rms: Vec<T::RMPath>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Wait<T: PayloadTypes> {
  pub query_id: QueryId,
  pub rm: T::RMPath,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Aborted<T: PayloadTypes> {
  pub query_id: QueryId,
  pub rm: T::RMPath,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum TMMessage<T: PayloadTypes> {
  Prepared(Prepared<T>),
  InformPrepared(InformPrepared<T>),
  Wait(Wait<T>),
  Aborted(Aborted<T>),
}

// -----------------------------------------------------------------------------------------------
//  Paxos2PCTMInner
// -----------------------------------------------------------------------------------------------

/// Contains callbacks for certain events in the TM.
pub trait Paxos2PCTMInner<T: PayloadTypes> {
  /// Called after all RMs have responded with `Prepared`.
  fn committed<IO: BasicIOCtx<T::NetworkMessageT>>(
    &mut self,
    ctx: &mut T::TMContext,
    io_ctx: &mut IO,
  );

  /// Called if one of the RMs returned `Aborted`.
  fn aborted<IO: BasicIOCtx<T::NetworkMessageT>>(
    &mut self,
    ctx: &mut T::TMContext,
    io_ctx: &mut IO,
  );

  // This is called when the node died.
  fn node_died<IO: BasicIOCtx<T::NetworkMessageT>>(
    &mut self,
    ctx: &mut T::TMContext,
    io_ctx: &mut IO,
  );
}

// -----------------------------------------------------------------------------------------------
//  Paxos2PCTMOuter
// -----------------------------------------------------------------------------------------------

#[derive(Debug)]
pub struct PreparingSt<T: PayloadTypes> {
  all_rms: Vec<T::RMPath>,
  /// Maps the RMs that have not responded to the `Prepare` messages we sent out.
  rms_remaining: BTreeMap<T::RMPath, Prepare<T>>,
}

#[derive(Debug)]
pub struct CheckingPreparedSt<T: PayloadTypes> {
  all_rms: Vec<T::RMPath>,
  /// Maps the RMs that have not responded to the `CheckPrepared` messages we sent out.
  rms_remaining: BTreeMap<T::RMPath, CheckPrepared<T>>,
}

#[derive(Debug)]
pub enum State<T: PayloadTypes> {
  Start,
  Preparing(PreparingSt<T>),
  CheckingPrepared(CheckingPreparedSt<T>),
}

pub enum Paxos2PCTMAction {
  Wait,
  Exit,
}

#[derive(Debug)]
pub struct Paxos2PCTMOuter<T: PayloadTypes, InnerT> {
  /// The `QueryId` identifying the Paxos2PC instance.
  pub query_id: QueryId,
  pub state: State<T>,
  pub inner: InnerT,
}

impl<T: PayloadTypes, InnerT: Paxos2PCTMInner<T>> Paxos2PCTMOuter<T, InnerT> {
  pub fn new(query_id: QueryId, inner: InnerT) -> Paxos2PCTMOuter<T, InnerT> {
    Paxos2PCTMOuter { query_id, state: State::Start, inner }
  }

  pub fn start_orig<IO: BasicIOCtx<T::NetworkMessageT>>(
    &mut self,
    ctx: &mut T::TMContext,
    io_ctx: &mut IO,
    prepare_payloads: BTreeMap<T::RMPath, T::Prepare>,
  ) -> Paxos2PCTMAction {
    // Send out FinishQueryPrepare to all RMs
    let all_rms: Vec<T::RMPath> = prepare_payloads.keys().cloned().collect();
    let mut rms_remaining = BTreeMap::<T::RMPath, Prepare<T>>::new();
    for (rm, payload) in prepare_payloads {
      let prepare = Prepare {
        query_id: self.query_id.clone(),
        tm: ctx.mk_node_path(),
        rms: all_rms.clone(),
        payload,
      };
      rms_remaining.insert(rm.clone(), prepare.clone());
      ctx.send_to_rm(io_ctx, &rm, T::rm_msg(RMMessage::Prepare(prepare)));
    }
    self.state = State::Preparing(PreparingSt { all_rms, rms_remaining });
    Paxos2PCTMAction::Wait
  }

  pub fn start_rec<IO: BasicIOCtx<T::NetworkMessageT>>(
    &mut self,
    ctx: &mut T::TMContext,
    io_ctx: &mut IO,
    all_rms: Vec<T::RMPath>,
  ) -> Paxos2PCTMAction {
    // Send out FinishQueryPrepare to all RMs
    let mut rms_remaining = BTreeMap::<T::RMPath, CheckPrepared<T>>::new();
    for rm in &all_rms {
      let check = CheckPrepared { query_id: self.query_id.clone(), tm: ctx.mk_node_path() };
      rms_remaining.insert(rm.clone(), check.clone());
      ctx.send_to_rm(io_ctx, &rm, T::rm_msg(RMMessage::CheckPrepared(check)));
    }
    self.state = State::CheckingPrepared(CheckingPreparedSt { all_rms, rms_remaining });
    Paxos2PCTMAction::Wait
  }

  // Paxos2PC messages

  /// Send out `Commit` and exit
  fn commit<IO: BasicIOCtx<T::NetworkMessageT>>(
    &mut self,
    ctx: &mut T::TMContext,
    io_ctx: &mut IO,
    all_rms: &Vec<T::RMPath>,
  ) -> Paxos2PCTMAction {
    for rm in all_rms {
      ctx.send_to_rm(
        io_ctx,
        rm,
        T::rm_msg(RMMessage::Commit(Commit {
          query_id: self.query_id.clone(),
          tm: ctx.mk_node_path(),
        })),
      )
    }
    self.inner.committed(ctx, io_ctx);
    Paxos2PCTMAction::Exit
  }

  pub fn handle_prepared<IO: BasicIOCtx<T::NetworkMessageT>>(
    &mut self,
    ctx: &mut T::TMContext,
    io_ctx: &mut IO,
    prepared: Prepared<T>,
  ) -> Paxos2PCTMAction {
    match &mut self.state {
      State::Preparing(PreparingSt { all_rms, rms_remaining }) => {
        rms_remaining.remove(&prepared.rm);
        if rms_remaining.is_empty() {
          let all_rms = all_rms.clone();
          self.commit(ctx, io_ctx, &all_rms)
        } else {
          Paxos2PCTMAction::Wait
        }
      }
      State::CheckingPrepared(CheckingPreparedSt { all_rms, rms_remaining }) => {
        rms_remaining.remove(&prepared.rm);
        if rms_remaining.is_empty() {
          let all_rms = all_rms.clone();
          self.commit(ctx, io_ctx, &all_rms)
        } else {
          Paxos2PCTMAction::Wait
        }
      }
      _ => Paxos2PCTMAction::Wait,
    }
  }

  pub fn handle_aborted<IO: BasicIOCtx<T::NetworkMessageT>>(
    &mut self,
    ctx: &mut T::TMContext,
    io_ctx: &mut IO,
  ) -> Paxos2PCTMAction {
    match &mut self.state {
      State::Preparing(PreparingSt { all_rms, .. })
      | State::CheckingPrepared(CheckingPreparedSt { all_rms, .. }) => {
        // The Preparing has been aborted.
        for rm in all_rms {
          ctx.send_to_rm(
            io_ctx,
            rm,
            T::rm_msg(RMMessage::Abort(Abort {
              query_id: self.query_id.clone(),
              tm: ctx.mk_node_path(),
            })),
          )
        }
        self.inner.aborted(ctx, io_ctx);
        Paxos2PCTMAction::Exit
      }
      _ => Paxos2PCTMAction::Wait,
    }
  }

  pub fn handle_wait<IO: BasicIOCtx<T::NetworkMessageT>>(
    &mut self,
    ctx: &mut T::TMContext,
    io_ctx: &mut IO,
    wait: Wait<T>,
  ) -> Paxos2PCTMAction {
    match &mut self.state {
      State::CheckingPrepared(CheckingPreparedSt { rms_remaining, .. }) => {
        // Send back a CheckPrepared
        let check = rms_remaining.get(&wait.rm).unwrap().clone();
        ctx.send_to_rm(io_ctx, &wait.rm, T::rm_msg(RMMessage::CheckPrepared(check.clone())));
        Paxos2PCTMAction::Wait
      }
      _ => Paxos2PCTMAction::Wait,
    }
  }

  pub fn remote_leader_changed<IO: BasicIOCtx<T::NetworkMessageT>>(
    &mut self,
    ctx: &mut T::TMContext,
    io_ctx: &mut IO,
    remote_leader_changed: RemoteLeaderChangedPLm,
  ) -> Paxos2PCTMAction {
    match &self.state {
      State::Preparing(PreparingSt { rms_remaining, .. }) => {
        for (rm, prepare) in rms_remaining {
          // If the RM has not responded and its Leadership changed, we resend Prepare.
          if rm.to_gid() == remote_leader_changed.gid {
            ctx.send_to_rm(io_ctx, rm, T::rm_msg(RMMessage::Prepare(prepare.clone())));
          }
        }
      }
      State::CheckingPrepared(CheckingPreparedSt { rms_remaining, .. }) => {
        for (rm, check) in rms_remaining {
          // If the RM has not responded and its Leadership changed, we resend CheckPrepared.
          if rm.to_gid() == remote_leader_changed.gid {
            ctx.send_to_rm(io_ctx, rm, T::rm_msg(RMMessage::CheckPrepared(check.clone())));
          }
        }
      }
      _ => {}
    }
    Paxos2PCTMAction::Wait
  }

  // TODO: employ node died. Add LeaderChanged to outer.
}

// -----------------------------------------------------------------------------------------------
//  Aggregate STM ES Management
// -----------------------------------------------------------------------------------------------
pub trait AggregateContainer<T: PayloadTypes, InnerT: Paxos2PCTMInner<T>> {
  fn get_mut(&mut self, query_id: &QueryId) -> Option<&mut Paxos2PCTMOuter<T, InnerT>>;

  fn insert(&mut self, query_id: QueryId, es: Paxos2PCTMOuter<T, InnerT>);
}

/// Implementation for BTreeMap, which is the common case.
impl<T: PayloadTypes, InnerT: Paxos2PCTMInner<T>> AggregateContainer<T, InnerT>
  for BTreeMap<QueryId, Paxos2PCTMOuter<T, InnerT>>
{
  fn get_mut(&mut self, query_id: &QueryId) -> Option<&mut Paxos2PCTMOuter<T, InnerT>> {
    self.get_mut(query_id)
  }

  fn insert(&mut self, query_id: QueryId, es: Paxos2PCTMOuter<T, InnerT>) {
    self.insert(query_id, es);
  }
}

/// Function to handle the arrive of an `TMMessage` for a given `AggregateContainer`.
pub fn handle_tm_msg<
  T: PayloadTypes,
  InnerT: Paxos2PCTMInner<T>,
  ConT: AggregateContainer<T, InnerT>,
  IO: BasicIOCtx<T::NetworkMessageT>,
>(
  ctx: &mut T::TMContext,
  io_ctx: &mut IO,
  con: &mut ConT,
  msg: TMMessage<T>,
) -> (QueryId, Paxos2PCTMAction) {
  match msg {
    TMMessage::Prepared(prepared) => {
      if let Some(es) = con.get_mut(&prepared.query_id) {
        (prepared.query_id.clone(), es.handle_prepared(ctx, io_ctx, prepared))
      } else {
        (prepared.query_id, Paxos2PCTMAction::Wait)
      }
    }
    TMMessage::Aborted(aborted) => {
      if let Some(es) = con.get_mut(&aborted.query_id) {
        (aborted.query_id.clone(), es.handle_aborted(ctx, io_ctx))
      } else {
        (aborted.query_id, Paxos2PCTMAction::Wait)
      }
    }
    TMMessage::InformPrepared(inform_prepared) => {
      // TODO: do
      (inform_prepared.query_id, Paxos2PCTMAction::Wait)
    }
    TMMessage::Wait(wait) => {
      if let Some(es) = con.get_mut(&wait.query_id) {
        (wait.query_id.clone(), es.handle_wait(ctx, io_ctx, wait))
      } else {
        (wait.query_id, Paxos2PCTMAction::Wait)
      }
    }
  }
}