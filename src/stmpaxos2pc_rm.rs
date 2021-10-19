use crate::alter_table_tm_es::{
  AlterTableClosed, AlterTableCommit, AlterTablePayloadTypes, AlterTablePrepared,
  AlterTableRMAborted, AlterTableRMCommitted, AlterTableRMPrepared,
};
use crate::common::CoreIOCtx;
use crate::model::common::{proc, QueryId, TNodePath, Timestamp};
use crate::server::ServerContextBase;
use crate::stmpaxos2pc_tm::{
  Closed, PayloadTypes, Prepared, RMAbortedPLm, RMCommittedPLm, RMPreparedPLm, TMCommittedPLm,
};
use crate::tablet::TabletContext;
use std::collections::HashMap;

// -----------------------------------------------------------------------------------------------
//  STMPaxos2PCRMInner
// -----------------------------------------------------------------------------------------------

pub trait STMPaxos2PCRMInner<T: PayloadTypes> {
  /// This is called at various times, like after `CommittedPLm` and `AbortedPLM` are inserted,
  /// and while in `WaitingInsertingPrepared`, when an `Abort` arrives.
  fn mk_closed<IO: CoreIOCtx>(&mut self, ctx: &mut TabletContext, io_ctx: &mut IO) -> T::Closed;

  /// Called in order to get the `RMPreparedPLm` to insert.
  fn mk_prepared_plm<IO: CoreIOCtx>(
    &mut self,
    ctx: &mut TabletContext,
    io_ctx: &mut IO,
  ) -> T::RMPreparedPLm;

  /// Called after PreparedPLm is inserted.
  fn prepared_plm_inserted<IO: CoreIOCtx>(
    &mut self,
    ctx: &mut TabletContext,
    io_ctx: &mut IO,
  ) -> T::Prepared;

  /// Called after all RMs have Prepared.
  fn mk_committed_plm<IO: CoreIOCtx>(
    &mut self,
    ctx: &mut TabletContext,
    io_ctx: &mut IO,
    commit: T::Commit,
  ) -> T::RMCommittedPLm;

  /// Called after CommittedPLm is inserted.
  fn committed_plm_inserted<IO: CoreIOCtx>(
    &mut self,
    ctx: &mut TabletContext,
    io_ctx: &mut IO,
    committed_plm: &RMCommittedPLm<T>,
  );

  /// Called if one of the RMs returned Aborted.
  fn mk_aborted_plm<IO: CoreIOCtx>(
    &mut self,
    ctx: &mut TabletContext,
    io_ctx: &mut IO,
  ) -> T::RMAbortedPLm;

  /// Called after AbortedPLm is inserted.
  fn aborted_plm_inserted<IO: CoreIOCtx>(&mut self, ctx: &mut TabletContext, io_ctx: &mut IO);
}

// -----------------------------------------------------------------------------------------------
//  STMPaxos2PCRMOuter
// -----------------------------------------------------------------------------------------------

#[derive(Debug)]
pub enum State<T: PayloadTypes> {
  Follower,
  WaitingInsertingPrepared,
  InsertingPrepared,
  Prepared(Prepared<T>),
  InsertingCommitted,
  InsertingPreparedAborted,
  InsertingAborted,
}

pub enum STMPaxos2PCRMAction {
  Wait,
  Exit,
}

#[derive(Debug)]
pub struct STMPaxos2PCRMOuter<T: PayloadTypes, InnerT> {
  pub query_id: QueryId,
  pub follower: Option<Prepared<T>>,
  pub state: State<T>,
  pub inner: InnerT,
}

impl<T: PayloadTypes, InnerT: STMPaxos2PCRMInner<T>> STMPaxos2PCRMOuter<T, InnerT> {
  // STMPaxos2PC messages

  pub fn handle_prepare<IO: CoreIOCtx>(
    &mut self,
    ctx: &mut TabletContext,
    io_ctx: &mut IO,
  ) -> STMPaxos2PCRMAction {
    match &self.state {
      State::Prepared(prepared) => {
        ctx.ctx(io_ctx).send_to_master(T::master_prepared(prepared.clone()));
      }
      _ => {}
    }
    STMPaxos2PCRMAction::Wait
  }

  pub fn handle_commit<IO: CoreIOCtx>(
    &mut self,
    ctx: &mut TabletContext,
    io_ctx: &mut IO,
    commit: T::Commit,
  ) -> STMPaxos2PCRMAction {
    match &self.state {
      State::Prepared(_) => {
        let committed_plm = T::tablet_committed_plm(RMCommittedPLm {
          query_id: self.query_id.clone(),
          payload: self.inner.mk_committed_plm(ctx, io_ctx, commit),
        });
        ctx.tablet_bundle.push(committed_plm);
        self.state = State::InsertingCommitted;
      }
      _ => {}
    }
    STMPaxos2PCRMAction::Wait
  }

  pub fn handle_abort<IO: CoreIOCtx>(
    &mut self,
    ctx: &mut TabletContext,
    io_ctx: &mut IO,
  ) -> STMPaxos2PCRMAction {
    match &self.state {
      State::WaitingInsertingPrepared => {
        self.send_closed(ctx, io_ctx);
        STMPaxos2PCRMAction::Exit
      }
      State::InsertingPrepared => {
        self.state = State::InsertingPreparedAborted;
        STMPaxos2PCRMAction::Wait
      }
      State::Prepared(_) => {
        let aborted_plm = T::tablet_aborted_plm(RMAbortedPLm {
          query_id: self.query_id.clone(),
          payload: self.inner.mk_aborted_plm(ctx, io_ctx),
        });
        ctx.tablet_bundle.push(aborted_plm);
        self.state = State::InsertingAborted;
        STMPaxos2PCRMAction::Wait
      }
      _ => STMPaxos2PCRMAction::Wait,
    }
  }

  // STMPaxos2PC PLm Insertions

  fn _handle_prepared_plm<IO: CoreIOCtx>(
    &mut self,
    ctx: &mut TabletContext,
    io_ctx: &mut IO,
  ) -> Prepared<T> {
    let this_node_path = ctx.mk_node_path();
    let prepared = Prepared {
      query_id: self.query_id.clone(),
      rm: this_node_path,
      payload: self.inner.prepared_plm_inserted(ctx, io_ctx),
    };
    self.follower = Some(prepared.clone());
    prepared
  }

  pub fn handle_prepared_plm<IO: CoreIOCtx>(
    &mut self,
    ctx: &mut TabletContext,
    io_ctx: &mut IO,
  ) -> STMPaxos2PCRMAction {
    match &self.state {
      State::InsertingPrepared => {
        let prepared = self._handle_prepared_plm(ctx, io_ctx);
        ctx.ctx(io_ctx).send_to_master(T::master_prepared(prepared.clone()));
        self.state = State::Prepared(prepared);
      }
      State::InsertingPreparedAborted => {
        self._handle_prepared_plm(ctx, io_ctx);
        let aborted_plm = T::tablet_aborted_plm(RMAbortedPLm {
          query_id: self.query_id.clone(),
          payload: self.inner.mk_aborted_plm(ctx, io_ctx),
        });
        ctx.tablet_bundle.push(aborted_plm);
        self.state = State::InsertingAborted;
      }
      _ => {}
    }
    STMPaxos2PCRMAction::Wait
  }

  pub fn handle_committed_plm<IO: CoreIOCtx>(
    &mut self,
    ctx: &mut TabletContext,
    io_ctx: &mut IO,
    committed_plm: RMCommittedPLm<T>,
  ) -> STMPaxos2PCRMAction {
    match &self.state {
      State::Follower => {
        self.inner.committed_plm_inserted(ctx, io_ctx, &committed_plm);
        STMPaxos2PCRMAction::Exit
      }
      State::InsertingCommitted => {
        self.inner.committed_plm_inserted(ctx, io_ctx, &committed_plm);
        self.send_closed(ctx, io_ctx);
        STMPaxos2PCRMAction::Exit
      }
      _ => STMPaxos2PCRMAction::Wait,
    }
  }

  pub fn handle_aborted_plm<IO: CoreIOCtx>(
    &mut self,
    ctx: &mut TabletContext,
    io_ctx: &mut IO,
  ) -> STMPaxos2PCRMAction {
    match &self.state {
      State::Follower => {
        self.inner.aborted_plm_inserted(ctx, io_ctx);
        STMPaxos2PCRMAction::Exit
      }
      State::InsertingAborted => {
        self.inner.aborted_plm_inserted(ctx, io_ctx);
        self.send_closed(ctx, io_ctx);
        STMPaxos2PCRMAction::Exit
      }
      _ => STMPaxos2PCRMAction::Wait,
    }
  }

  // Other

  pub fn start_inserting<IO: CoreIOCtx>(
    &mut self,
    ctx: &mut TabletContext,
    io_ctx: &mut IO,
  ) -> STMPaxos2PCRMAction {
    match &self.state {
      State::WaitingInsertingPrepared => {
        let prepared_plm = RMPreparedPLm {
          query_id: self.query_id.clone(),
          payload: self.inner.mk_prepared_plm(ctx, io_ctx),
        };
        ctx.tablet_bundle.push(T::tablet_prepared_plm(prepared_plm));
        self.state = State::InsertingPrepared;
      }
      _ => {}
    }
    STMPaxos2PCRMAction::Wait
  }

  pub fn leader_changed(&mut self, ctx: &mut TabletContext) -> STMPaxos2PCRMAction {
    match &self.state {
      State::Follower => {
        if ctx.is_leader() {
          let prepared = self.follower.as_ref().unwrap();
          self.state = State::Prepared(prepared.clone());
        }
        STMPaxos2PCRMAction::Wait
      }
      State::WaitingInsertingPrepared
      | State::InsertingPrepared
      | State::InsertingPreparedAborted => STMPaxos2PCRMAction::Exit,
      State::Prepared(_) | State::InsertingCommitted | State::InsertingAborted => {
        self.state = State::Follower;
        STMPaxos2PCRMAction::Wait
      }
    }
  }

  // Helpers
  pub fn send_closed<IO: CoreIOCtx>(&mut self, ctx: &mut TabletContext, io_ctx: &mut IO) {
    let this_node_path = ctx.mk_node_path();
    let closed = Closed {
      query_id: self.query_id.clone(),
      rm: this_node_path,
      payload: self.inner.mk_closed(ctx, io_ctx),
    };
    ctx.ctx(io_ctx).send_to_master(T::master_closed(closed));
  }
}

// -----------------------------------------------------------------------------------------------
//  AlterTableES Implementation
// -----------------------------------------------------------------------------------------------

#[derive(Debug)]
pub struct AlterTableRMInner {
  pub query_id: QueryId,
  pub alter_op: proc::AlterOp,
  pub prepared_timestamp: Timestamp,
}

impl STMPaxos2PCRMInner<AlterTablePayloadTypes> for AlterTableRMInner {
  fn mk_closed<IO: CoreIOCtx>(&mut self, _: &mut TabletContext, _: &mut IO) -> AlterTableClosed {
    AlterTableClosed {}
  }

  fn mk_prepared_plm<IO: CoreIOCtx>(
    &mut self,
    _: &mut TabletContext,
    _: &mut IO,
  ) -> AlterTableRMPrepared {
    AlterTableRMPrepared { alter_op: self.alter_op.clone(), timestamp: self.prepared_timestamp }
  }

  fn prepared_plm_inserted<IO: CoreIOCtx>(
    &mut self,
    _: &mut TabletContext,
    _: &mut IO,
  ) -> AlterTablePrepared {
    AlterTablePrepared { timestamp: self.prepared_timestamp }
  }

  fn mk_committed_plm<IO: CoreIOCtx>(
    &mut self,
    _: &mut TabletContext,
    _: &mut IO,
    commit: AlterTableCommit,
  ) -> AlterTableRMCommitted {
    AlterTableRMCommitted { timestamp: commit.timestamp }
  }
  /// Apply the `alter_op` to this Tablet's `table_schema`.
  fn committed_plm_inserted<IO: CoreIOCtx>(
    &mut self,
    ctx: &mut TabletContext,
    _: &mut IO,
    committed_plm: &RMCommittedPLm<AlterTablePayloadTypes>,
  ) {
    ctx.table_schema.val_cols.write(
      &self.alter_op.col_name,
      self.alter_op.maybe_col_type.clone(),
      committed_plm.payload.timestamp,
    );
  }

  fn mk_aborted_plm<IO: CoreIOCtx>(
    &mut self,
    _: &mut TabletContext,
    _: &mut IO,
  ) -> AlterTableRMAborted {
    AlterTableRMAborted {}
  }

  fn aborted_plm_inserted<IO: CoreIOCtx>(&mut self, _: &mut TabletContext, _: &mut IO) {}
}