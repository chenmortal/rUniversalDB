use crate::common::{MasterIOCtx, RemoteLeaderChangedPLm};
use crate::create_table_tm_es::ResponseData;
use crate::master::{MasterContext, MasterPLm};
use crate::model::common::{proc, QueryId, TNodePath, TSubNodePath, TablePath, Timestamp};
use crate::model::message as msg;
use crate::server::ServerContextBase;
use serde::{Deserialize, Serialize};
use std::cmp::max;
use std::collections::{HashMap, HashSet};
use std::fmt::Debug;

// -----------------------------------------------------------------------------------------------
//  STMPaxos2PC
// -----------------------------------------------------------------------------------------------

pub trait PayloadTypes: Sized {
  // TM PLm
  type TMPreparedPLm: Debug + Clone;
  type TMCommittedPLm: Debug + Clone;
  type TMAbortedPLm: Debug + Clone;
  type TMClosedPLm: Debug + Clone;

  fn master_prepared_plm(prepared_plm: TMPreparedPLm<Self>) -> MasterPLm;
  fn master_committed_plm(committed_plm: TMCommittedPLm<Self>) -> MasterPLm;
  fn master_aborted_plm(aborted_plm: TMAbortedPLm<Self>) -> MasterPLm;
  fn master_closed_plm(closed_plm: TMClosedPLm<Self>) -> MasterPLm;

  // RM PLm
  type RMPreparedPLm: Debug + Clone;
  type RMCommittedPLm: Debug + Clone;
  type RMAbortedPLm: Debug + Clone;

  // TM-to-RM Messages
  type Prepare: Debug + Clone;
  type Abort: Debug + Clone;
  type Commit: Debug + Clone;

  fn tablet_prepare(prepare: Prepare<Self>) -> msg::TabletMessage;
  fn tablet_commit(commit: Commit<Self>) -> msg::TabletMessage;
  fn tablet_abort(abort: Abort<Self>) -> msg::TabletMessage;

  // RM-to-TM Messages
  type Prepared: Debug + Clone;
  type Aborted: Debug + Clone;
  type Closed: Debug + Clone;
}

// TM PLm
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TMPreparedPLm<T: PayloadTypes> {
  pub query_id: QueryId,
  pub payload: T::TMPreparedPLm,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TMCommittedPLm<T: PayloadTypes> {
  pub query_id: QueryId,
  pub payload: T::TMCommittedPLm,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TMAbortedPLm<T: PayloadTypes> {
  pub query_id: QueryId,
  pub payload: T::TMAbortedPLm,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TMClosedPLm<T: PayloadTypes> {
  pub query_id: QueryId,
  pub payload: T::TMClosedPLm,
}

// RM PLm
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RMPreparedPLm<T: PayloadTypes> {
  pub query_id: QueryId,
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

// TM-to-RM Messages
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Prepare<T: PayloadTypes> {
  pub query_id: QueryId,
  pub payload: T::Prepare,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Abort<T: PayloadTypes> {
  pub query_id: QueryId,
  pub payload: T::Abort,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Commit<T: PayloadTypes> {
  pub query_id: QueryId,
  pub payload: T::Commit,
}

// RM-to-TM Messages
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Prepared<T: PayloadTypes> {
  pub query_id: QueryId,
  pub rm: TNodePath,
  pub payload: T::Prepared,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Aborted<T: PayloadTypes> {
  pub query_id: QueryId,
  pub payload: T::Aborted,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Closed<T: PayloadTypes> {
  pub query_id: QueryId,
  pub rm: TNodePath,
  pub payload: T::Closed,
}

// -----------------------------------------------------------------------------------------------
//  STMPaxos2PCTM Inner
// -----------------------------------------------------------------------------------------------

pub trait STMPaxos2PCTMInner<T: PayloadTypes> {
  /// Called in order to get the `TMPreparedPLm` to insert.
  fn mk_prepared_plm<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
  ) -> T::TMPreparedPLm;

  /// Called after PreparedPLm is inserted.
  fn prepared_plm_inserted<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
  ) -> HashMap<TNodePath, T::Prepare>;

  /// Called after all RMs have Prepared.
  fn mk_committed_plm<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
    prepared: &HashMap<TNodePath, T::Prepared>,
  ) -> T::TMCommittedPLm;

  /// Called after CommittedPLm is inserted.
  fn committed_plm_inserted<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
    committed_plm: &TMCommittedPLm<T>,
  ) -> HashMap<TNodePath, T::Commit>;

  /// Called if one of the RMs returned Aborted.
  fn mk_aborted_plm<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
  ) -> T::TMAbortedPLm;

  /// Called after AbortedPLm is inserted.
  fn aborted_plm_inserted<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
  ) -> HashMap<TNodePath, T::Abort>;

  /// Called after all RMs have processed the `Commit` or or `Abort` message.
  fn mk_closed_plm<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
  ) -> T::TMClosedPLm;

  /// Called after all RMs have processed the `Commit` or or `Abort` message.
  fn closed_plm_inserted<IO: MasterIOCtx>(&mut self, ctx: &mut MasterContext, io_ctx: &mut IO);

  // This is called when the node died.
  fn node_died<IO: MasterIOCtx>(&mut self, ctx: &mut MasterContext, io_ctx: &mut IO);
}

// -----------------------------------------------------------------------------------------------
//  STMPaxos2PCTM Outer
// -----------------------------------------------------------------------------------------------

#[derive(Debug)]
pub struct PreparingSt<T: PayloadTypes> {
  rms_remaining: HashSet<TNodePath>,
  prepared: HashMap<TNodePath, T::Prepared>,
}

#[derive(Debug)]
pub struct CommittedSt {
  rms_remaining: HashSet<TNodePath>,
}

#[derive(Debug)]
pub struct AbortedSt {
  rms_remaining: HashSet<TNodePath>,
}

#[derive(Debug)]
pub enum FollowerState<T: PayloadTypes> {
  Preparing(HashMap<TNodePath, T::Prepare>),
  Committed(HashMap<TNodePath, T::Commit>),
  Aborted(HashMap<TNodePath, T::Abort>),
}

#[derive(Debug)]
pub enum State<T: PayloadTypes> {
  Following,
  Start,
  WaitingInsertTMPrepared,
  InsertTMPreparing,
  Preparing(PreparingSt<T>),
  InsertingTMCommitted,
  Committed(CommittedSt),
  InsertingTMAborted,
  Aborted(AbortedSt),
  InsertingTMClosed,
}

pub enum STMPaxos2PCAction {
  Wait,
  Exit,
}

#[derive(Debug)]
pub struct STMPaxos2PCOuter<T: PayloadTypes, InnerT> {
  pub query_id: QueryId,
  pub follower: Option<FollowerState<T>>,
  pub state: State<T>,
  pub inner: InnerT,
}

impl<T: PayloadTypes, InnerT: STMPaxos2PCTMInner<T>> STMPaxos2PCOuter<T, InnerT> {
  pub fn new(query_id: QueryId, inner: InnerT) -> STMPaxos2PCOuter<T, InnerT> {
    STMPaxos2PCOuter { query_id, follower: None, state: State::Start, inner }
  }

  /// This is only called when the `PreparedPLm` is insert at a Follower node.
  pub fn init_follower<IO: MasterIOCtx>(&mut self, ctx: &mut MasterContext, io_ctx: &mut IO) {
    let prepare_payloads = self.inner.prepared_plm_inserted(ctx, io_ctx);
    self.follower = Some(FollowerState::Preparing(prepare_payloads));
    self.state = State::Following;
  }

  // STMPaxos2PC messages

  pub fn handle_prepared<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
    prepared: Prepared<T>,
  ) -> STMPaxos2PCAction {
    match &mut self.state {
      State::Preparing(preparing) => {
        if preparing.rms_remaining.remove(&prepared.rm) {
          preparing.prepared.insert(prepared.rm.clone(), prepared.payload);
          if preparing.rms_remaining.is_empty() {
            let committed_plm = T::master_committed_plm(TMCommittedPLm {
              query_id: self.query_id.clone(),
              payload: self.inner.mk_committed_plm(ctx, io_ctx, &preparing.prepared),
            });
            ctx.master_bundle.plms.push(committed_plm);
            self.state = State::InsertingTMCommitted;
          }
        }
      }
      _ => {}
    }
    STMPaxos2PCAction::Wait
  }

  pub fn handle_aborted<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
  ) -> STMPaxos2PCAction {
    match &mut self.state {
      State::Preparing(_) => {
        let aborted_plm = T::master_aborted_plm(TMAbortedPLm {
          query_id: self.query_id.clone(),
          payload: self.inner.mk_aborted_plm(ctx, io_ctx),
        });
        ctx.master_bundle.plms.push(aborted_plm);
        self.state = State::InsertingTMAborted;
      }
      _ => {}
    }
    STMPaxos2PCAction::Wait
  }

  pub fn handle_close_confirmed<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
    closed: Closed<T>,
  ) -> STMPaxos2PCAction {
    match &mut self.state {
      State::Committed(committed) => {
        if committed.rms_remaining.remove(&closed.rm) {
          if committed.rms_remaining.is_empty() {
            // All RMs have committed
            let closed_plm = T::master_closed_plm(TMClosedPLm {
              query_id: self.query_id.clone(),
              payload: self.inner.mk_closed_plm(ctx, io_ctx),
            });
            ctx.master_bundle.plms.push(closed_plm);
            self.state = State::InsertingTMClosed;
          }
        }
      }
      State::Aborted(aborted) => {
        if aborted.rms_remaining.remove(&closed.rm) {
          if aborted.rms_remaining.is_empty() {
            // All RMs have aborted
            let closed_plm = T::master_closed_plm(TMClosedPLm {
              query_id: self.query_id.clone(),
              payload: self.inner.mk_closed_plm(ctx, io_ctx),
            });
            ctx.master_bundle.plms.push(closed_plm);
            self.state = State::InsertingTMClosed;
          }
        }
      }
      _ => {}
    }
    STMPaxos2PCAction::Wait
  }

  // STMPaxos2PC PLm Insertions

  /// Change state to `Preparing` and broadcast `Prepare` to the RMs.
  fn advance_to_prepared<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
    prepare_payloads: HashMap<TNodePath, T::Prepare>,
  ) {
    let mut rms_remaining = HashSet::<TNodePath>::new();
    for (rm, payload) in prepare_payloads.clone() {
      let prepare = Prepare { query_id: self.query_id.clone(), payload };
      ctx.ctx(io_ctx).send_to_t(rm.clone(), T::tablet_prepare(prepare));
      rms_remaining.insert(rm);
    }

    self.follower = Some(FollowerState::Preparing(prepare_payloads));
    self.state = State::Preparing(PreparingSt { rms_remaining, prepared: Default::default() });
  }

  pub fn handle_prepared_plm<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
  ) -> STMPaxos2PCAction {
    match &self.state {
      State::InsertTMPreparing => {
        let prepare_payloads = self.inner.prepared_plm_inserted(ctx, io_ctx);
        self.advance_to_prepared(ctx, io_ctx, prepare_payloads);
      }
      _ => {}
    }
    STMPaxos2PCAction::Wait
  }

  /// Change state to `Committed` and broadcast `AlterTableCommit` to the RMs.
  fn advance_to_committed<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
    commit_payloads: HashMap<TNodePath, T::Commit>,
  ) {
    let mut rms_remaining = HashSet::<TNodePath>::new();
    for (rm, payload) in commit_payloads.clone() {
      let commit = Commit { query_id: self.query_id.clone(), payload };
      ctx.ctx(io_ctx).send_to_t(rm.clone(), T::tablet_commit(commit));
      rms_remaining.insert(rm);
    }

    self.follower = Some(FollowerState::Committed(commit_payloads));
    self.state = State::Committed(CommittedSt { rms_remaining });
  }

  pub fn handle_committed_plm<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
    committed_plm: TMCommittedPLm<T>,
  ) -> STMPaxos2PCAction {
    match &self.state {
      State::Following => {
        let commit_payloads = self.inner.committed_plm_inserted(ctx, io_ctx, &committed_plm);
        self.follower = Some(FollowerState::Committed(commit_payloads));
      }
      State::InsertingTMCommitted => {
        let commit_payloads = self.inner.committed_plm_inserted(ctx, io_ctx, &committed_plm);
        self.follower = Some(FollowerState::Committed(commit_payloads.clone()));

        // Change state to Committed
        self.advance_to_committed(ctx, io_ctx, commit_payloads);

        // Broadcast a GossipData
        ctx.broadcast_gossip(io_ctx);
      }
      _ => {}
    }
    STMPaxos2PCAction::Wait
  }

  /// Change state to `Aborted` and broadcast `AlterTableAbort` to the RMs.
  fn advance_to_aborted<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
    abort_payloads: HashMap<TNodePath, T::Abort>,
  ) {
    let mut rms_remaining = HashSet::<TNodePath>::new();
    for (rm, payload) in abort_payloads.clone() {
      let abort = Abort { query_id: self.query_id.clone(), payload };
      ctx.ctx(io_ctx).send_to_t(rm.clone(), T::tablet_abort(abort));
      rms_remaining.insert(rm);
    }

    self.follower = Some(FollowerState::Aborted(abort_payloads));
    self.state = State::Aborted(AbortedSt { rms_remaining });
  }

  pub fn handle_aborted_plm<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
  ) -> STMPaxos2PCAction {
    match &self.state {
      State::Following => {
        let abort_payloads = self.inner.aborted_plm_inserted(ctx, io_ctx);
        self.follower = Some(FollowerState::Aborted(abort_payloads));
      }
      State::InsertingTMAborted => {
        let abort_payloads = self.inner.aborted_plm_inserted(ctx, io_ctx);
        self.follower = Some(FollowerState::Aborted(abort_payloads.clone()));

        // Change state to Aborted
        self.advance_to_aborted(ctx, io_ctx, abort_payloads);
      }
      _ => {}
    }
    STMPaxos2PCAction::Wait
  }

  /// Simply return Exit in the appropriate states.
  pub fn handle_closed_plm<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
  ) -> STMPaxos2PCAction {
    match &self.state {
      State::Following => {
        self.inner.closed_plm_inserted(ctx, io_ctx);
        STMPaxos2PCAction::Exit
      }
      State::InsertingTMClosed => {
        self.inner.closed_plm_inserted(ctx, io_ctx);
        STMPaxos2PCAction::Exit
      }
      _ => STMPaxos2PCAction::Wait,
    }
  }

  // Other

  pub fn start_inserting<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
  ) -> STMPaxos2PCAction {
    match &self.state {
      State::WaitingInsertTMPrepared => {
        let prepared = T::master_prepared_plm(TMPreparedPLm {
          query_id: self.query_id.clone(),
          payload: self.inner.mk_prepared_plm(ctx, io_ctx),
        });
        ctx.master_bundle.plms.push(prepared);
        self.state = State::InsertTMPreparing;
      }
      _ => {}
    }
    STMPaxos2PCAction::Wait
  }

  pub fn leader_changed<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
  ) -> STMPaxos2PCAction {
    match &self.state {
      State::Following => {
        if ctx.is_leader() {
          // Recall that if State was ever set to `Following`, then `follower` must have been set.
          match self.follower.as_ref().unwrap() {
            FollowerState::Preparing(prepare_payloads) => {
              self.advance_to_prepared(ctx, io_ctx, prepare_payloads.clone())
            }
            FollowerState::Committed(commit_payloads) => {
              self.advance_to_committed(ctx, io_ctx, commit_payloads.clone())
            }
            FollowerState::Aborted(abort_payloads) => {
              self.advance_to_aborted(ctx, io_ctx, abort_payloads.clone())
            }
          }
        }
        STMPaxos2PCAction::Wait
      }
      State::Start | State::WaitingInsertTMPrepared | State::InsertTMPreparing => {
        self.inner.node_died(ctx, io_ctx);
        STMPaxos2PCAction::Exit
      }
      State::Preparing(_)
      | State::InsertingTMCommitted
      | State::Committed(_)
      | State::InsertingTMAborted
      | State::Aborted(_)
      | State::InsertingTMClosed => {
        self.state = State::Following;
        self.inner.node_died(ctx, io_ctx);
        STMPaxos2PCAction::Wait
      }
    }
  }

  pub fn remote_leader_changed<IO: MasterIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
    remote_leader_changed: RemoteLeaderChangedPLm,
  ) -> STMPaxos2PCAction {
    let follower = self.follower.as_ref().unwrap();
    match &self.state {
      State::Preparing(preparing) => {
        let prepare_payloads = cast!(FollowerState::Preparing, follower).unwrap();
        for rm in &preparing.rms_remaining {
          // If the RM has not responded and its Leadership changed, we resend Prepare.
          if rm.sid.to_gid() == remote_leader_changed.gid {
            let payload = prepare_payloads.get(rm).unwrap().clone();
            let prepare = Prepare { query_id: self.query_id.clone(), payload };
            ctx.ctx(io_ctx).send_to_t(rm.clone(), T::tablet_prepare(prepare));
          }
        }
      }
      State::Committed(committed) => {
        let commit_payloads = cast!(FollowerState::Committed, follower).unwrap();
        for rm in &committed.rms_remaining {
          // If the RM has not responded and its Leadership changed, we resend Commit.
          if rm.sid.to_gid() == remote_leader_changed.gid {
            let payload = commit_payloads.get(rm).unwrap().clone();
            let commit = Commit { query_id: self.query_id.clone(), payload };
            ctx.ctx(io_ctx).send_to_t(rm.clone(), T::tablet_commit(commit));
          }
        }
      }
      State::Aborted(aborted) => {
        let abort_payloads = cast!(FollowerState::Aborted, follower).unwrap();
        for rm in &aborted.rms_remaining {
          // If the RM has not responded and its Leadership changed, we resend Abort.
          if rm.sid.to_gid() == remote_leader_changed.gid {
            let payload = abort_payloads.get(rm).unwrap().clone();
            let abort = Abort { query_id: self.query_id.clone(), payload };
            ctx.ctx(io_ctx).send_to_t(rm.clone(), T::tablet_abort(abort));
          }
        }
      }
      _ => {}
    }
    STMPaxos2PCAction::Wait
  }
}
