use crate::common::RemoteLeaderChangedPLm;
use crate::common::{IOTypes, NetworkOut};
use crate::coord::CoordContext;
use crate::model::common::{
  proc, EndpointId, QueryId, RequestId, TQueryPath, TableView, Timestamp,
};
use crate::model::message as msg;
use crate::server::ServerContextBase;
use std::collections::{HashMap, HashSet};

// -----------------------------------------------------------------------------------------------
//  FinishQueryOrigTMES
// -----------------------------------------------------------------------------------------------

#[derive(Debug)]
pub struct ResponseData {
  // Request values (values send in the original request)
  pub request_id: RequestId,
  pub sender_eid: EndpointId,
  /// We hold onto the original `MSQuery` in case of an Abort so that we can restart.
  pub sql_query: proc::MSQuery,

  // Result values (values computed by the MSCoordES)
  pub table_view: TableView,
  pub timestamp: Timestamp,
}

#[derive(Debug)]
pub enum Paxos2PCTMState {
  Start,
  // These holds the set of remaining RMs.
  Preparing(HashSet<TQueryPath>),
  CheckPreparing(HashSet<TQueryPath>),
}

#[derive(Debug)]
pub struct FinishQueryTMES {
  pub response_data: Option<ResponseData>,
  pub query_id: QueryId,
  pub all_rms: Vec<TQueryPath>,
  pub state: Paxos2PCTMState,
}

pub enum FinishQueryTMAction {
  Wait,
  Committed,
  // This can only happen if there was a badly timed  Leadership change of the RMs
  // (before it could persist the UpdateView). The whole MSQuery should be tried again.
  Aborted,
}

// -----------------------------------------------------------------------------------------------
//  Implementation
// -----------------------------------------------------------------------------------------------
impl FinishQueryTMES {
  pub fn start_orig<T: IOTypes>(&mut self, ctx: &mut CoordContext<T>) -> FinishQueryTMAction {
    // Send out FinishQueryPrepare to all RMs
    let mut state = HashSet::<TQueryPath>::new();
    for rm in &self.all_rms {
      state.insert(rm.clone());
      send_prepare(ctx, self.query_id.clone(), rm.clone(), self.all_rms.clone());
    }
    self.state = Paxos2PCTMState::Preparing(state);
    FinishQueryTMAction::Wait
  }

  pub fn start_rec<T: IOTypes>(&mut self, ctx: &mut CoordContext<T>) -> FinishQueryTMAction {
    // Send out FinishQueryCheckPrepared to all RMs
    let mut state = HashSet::<TQueryPath>::new();
    for rm in &self.all_rms {
      state.insert(rm.clone());
      send_check_prepared(ctx, self.query_id.clone(), rm.clone());
    }
    self.state = Paxos2PCTMState::CheckPreparing(state);
    FinishQueryTMAction::Wait
  }

  pub fn handle_prepared<T: IOTypes>(
    &mut self,
    ctx: &mut CoordContext<T>,
    prepared: msg::FinishQueryPrepared,
  ) -> FinishQueryTMAction {
    match &mut self.state {
      Paxos2PCTMState::Preparing(exec_state) | Paxos2PCTMState::CheckPreparing(exec_state) => {
        exec_state.remove(&prepared.rm_path);
        if exec_state.is_empty() {
          // The Preparing is finished.
          for rm in &self.all_rms {
            ctx.ctx().send_to_t(
              rm.node_path.clone(),
              msg::TabletMessage::FinishQueryCommit(msg::FinishQueryCommit {
                query_id: rm.query_id.clone(),
              }),
            )
          }
          FinishQueryTMAction::Committed
        } else {
          FinishQueryTMAction::Wait
        }
      }
      _ => FinishQueryTMAction::Wait,
    }
  }

  pub fn handle_aborted<T: IOTypes>(
    &mut self,
    ctx: &mut CoordContext<T>,
    aborted: msg::FinishQueryAborted,
  ) -> FinishQueryTMAction {
    match &mut self.state {
      Paxos2PCTMState::Preparing(exec_state) | Paxos2PCTMState::CheckPreparing(exec_state) => {
        exec_state.remove(&aborted.rm_path);
        // The Preparing has been aborted.
        for rm in &self.all_rms {
          ctx.ctx().send_to_t(
            rm.node_path.clone(),
            msg::TabletMessage::FinishQueryAbort(msg::FinishQueryAbort {
              query_id: rm.query_id.clone(),
            }),
          )
        }
        FinishQueryTMAction::Aborted
      }
      _ => FinishQueryTMAction::Wait,
    }
  }

  pub fn handle_wait<T: IOTypes>(
    &mut self,
    ctx: &mut CoordContext<T>,
    wait: msg::FinishQueryWait,
  ) -> FinishQueryTMAction {
    match &mut self.state {
      Paxos2PCTMState::CheckPreparing(_) => {
        // Send back a CheckPrepared
        send_check_prepared(ctx, self.query_id.clone(), wait.rm_path);
        FinishQueryTMAction::Aborted
      }
      _ => FinishQueryTMAction::Wait,
    }
  }

  pub fn remote_leader_changed<T: IOTypes>(
    &mut self,
    ctx: &mut CoordContext<T>,
    remote_leader_changed: RemoteLeaderChangedPLm,
  ) -> FinishQueryTMAction {
    match &self.state {
      Paxos2PCTMState::Preparing(exec_state) => {
        for rm in exec_state {
          // If the RM has not responded and its Leadership changed, we resend Prepare.
          if rm.node_path.sid.to_gid() == remote_leader_changed.gid {
            send_prepare(ctx, self.query_id.clone(), rm.clone(), self.all_rms.clone());
          }
        }
      }
      Paxos2PCTMState::CheckPreparing(exec_state) => {
        for rm in exec_state {
          // If the RM has not responded and its Leadership changed, we resend CheckPrepared.
          if rm.node_path.sid.to_gid() == remote_leader_changed.gid {
            send_check_prepared(ctx, self.query_id.clone(), rm.clone());
          }
        }
      }
      _ => {}
    }
    FinishQueryTMAction::Wait
  }
}

/// Send a `FinishQueryPrepare` to `rm`.
fn send_prepare<T: IOTypes>(
  ctx: &mut CoordContext<T>,
  this_query_id: QueryId,
  rm: TQueryPath,
  all_rms: Vec<TQueryPath>,
) {
  let tm = ctx.mk_query_path(this_query_id);
  ctx.ctx().send_to_t(
    rm.node_path.clone(),
    msg::TabletMessage::FinishQueryPrepare(msg::FinishQueryPrepare {
      tm,
      all_rms,
      query_id: rm.query_id.clone(),
    }),
  )
}

/// Send a `FinishQueryCheckPrepared` to `rm`.
fn send_check_prepared<T: IOTypes>(
  ctx: &mut CoordContext<T>,
  this_query_id: QueryId,
  rm: TQueryPath,
) {
  let tm = ctx.mk_query_path(this_query_id);
  ctx.ctx().send_to_t(
    rm.node_path.clone(),
    msg::TabletMessage::FinishQueryCheckPrepared(msg::FinishQueryCheckPrepared {
      tm,
      query_id: rm.query_id.clone(),
    }),
  )
}