use crate::alter_table_tm_es::ResponseData;
use crate::common::{
  cur_timestamp, mk_t, BasicIOCtx, FullGen, GeneralTraceMessage, GossipDataMutView, TableSchema,
  Timestamp,
};
use crate::common::{
  ColName, ColType, Gen, ShardingGen, SlaveGroupId, TablePath, TabletGroupId, TabletKeyRange,
};
use crate::master::{MasterContext, MasterPLm};
use crate::message as msg;
use crate::multiversion_map::MVM;
use crate::server::ServerContextBase;
use crate::slave::{SlaveContext, SlavePLm};
use crate::stmpaxos2pc_tm::{
  RMMessage, STMPaxos2PCTMInner, STMPaxos2PCTMOuter, TMClosedPLm, TMCommittedPLm, TMMessage, TMPLm,
  TMPayloadTypes, TMServerContext,
};
use crate::tablet::TabletCreateHelper;
use serde::{Deserialize, Serialize};
use std::cmp::max;
use std::collections::BTreeMap;

// -----------------------------------------------------------------------------------------------
//  Payloads
// -----------------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CreateTableTMPayloadTypes {}

impl TMPayloadTypes for CreateTableTMPayloadTypes {
  // Master
  type RMPath = SlaveGroupId;
  type TMPath = ();
  type NetworkMessageT = msg::NetworkMessage;
  type TMContext = MasterContext;

  // TM PLm
  type TMPreparedPLm = CreateTableTMPrepared;
  type TMCommittedPLm = CreateTableTMCommitted;
  type TMAbortedPLm = CreateTableTMAborted;
  type TMClosedPLm = CreateTableTMClosed;

  // TM-to-RM Messages
  type Prepare = CreateTablePrepare;
  type Abort = CreateTableAbort;
  type Commit = CreateTableCommit;

  // RM-to-TM Messages
  type Prepared = CreateTablePrepared;
  type Aborted = CreateTableAborted;
  type Closed = CreateTableClosed;
}

// TM PLm

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CreateTableTMPrepared {
  pub table_path: TablePath,
  pub key_cols: Vec<(ColName, ColType)>,
  pub val_cols: Vec<(ColName, ColType)>,
  pub shards: Vec<(TabletKeyRange, TabletGroupId, SlaveGroupId)>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CreateTableTMCommitted {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CreateTableTMAborted {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CreateTableTMClosed {
  pub timestamp_hint: Option<Timestamp>,
}

// TM-to-RM

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CreateTablePrepare {
  /// Randomly generated by the Master for use by the new Tablet.
  pub tablet_group_id: TabletGroupId,
  /// The `TablePath` of the new Tablet
  pub table_path: TablePath,
  /// The `Gen` of the new Table.
  pub gen: Gen,
  /// The KeyRange that the Tablet created by this should be servicing.
  pub key_range: TabletKeyRange,

  /// The initial schema of the Table.
  pub key_cols: Vec<(ColName, ColType)>,
  pub val_cols: Vec<(ColName, ColType)>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CreateTableAbort {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CreateTableCommit {}

// RM-to-TM

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CreateTablePrepared {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CreateTableAborted {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CreateTableClosed {}

// -----------------------------------------------------------------------------------------------
//  TMServerContext CreateTable
// -----------------------------------------------------------------------------------------------

impl TMServerContext<CreateTableTMPayloadTypes> for MasterContext {
  fn push_plm(&mut self, plm: TMPLm<CreateTableTMPayloadTypes>) {
    self.master_bundle.plms.push(MasterPLm::CreateTable(plm));
  }

  fn send_to_rm<IO: BasicIOCtx>(
    &mut self,
    io_ctx: &mut IO,
    rm: &SlaveGroupId,
    msg: RMMessage<CreateTableTMPayloadTypes>,
  ) {
    self.send_to_slave_common(io_ctx, rm.clone(), msg::SlaveRemotePayload::CreateTable(msg));
  }

  fn mk_node_path(&self) -> () {
    ()
  }

  fn is_leader(&self) -> bool {
    MasterContext::is_leader(self)
  }
}

// -----------------------------------------------------------------------------------------------
//  CreateTable Implementation
// -----------------------------------------------------------------------------------------------

pub type CreateTableTMES = STMPaxos2PCTMOuter<CreateTableTMPayloadTypes, CreateTableTMInner>;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CreateTableTMInner {
  // Response data
  pub response_data: Option<ResponseData>,

  // CreateTable Query data
  pub table_path: TablePath,
  pub key_cols: Vec<(ColName, ColType)>,
  pub val_cols: Vec<(ColName, ColType)>,
  pub shards: Vec<(TabletKeyRange, TabletGroupId, SlaveGroupId)>,

  /// This is set when `Committed` or `Aborted` gets inserted
  /// for use when constructing `Closed`.
  pub did_commit: bool,
}

impl CreateTableTMInner {
  /// Create the Table and return the `Timestamp` at which the Table has been created
  /// (based on the `timestamp_hint` and from GossipData).
  fn apply_create<IO: BasicIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    _: &mut IO,
    timestamp_hint: Timestamp,
  ) -> Timestamp {
    ctx.gossip.update(|gossip| {
      let commit_timestamp =
        max(timestamp_hint, gossip.table_generation.get_lat(&self.table_path).add(mk_t(1)));
      let gen = next_gen(gossip.table_generation.get_last_present_version(&self.table_path));
      let full_gen = (gen.clone(), Gen(0));

      // Update `table_generation`
      gossip.table_generation.write(
        &self.table_path,
        Some(full_gen.clone()),
        commit_timestamp.clone(),
      );

      // Update `db_schema`
      let table_path_gen = (self.table_path.clone(), gen);
      debug_assert!(!gossip.db_schema.contains_key(&table_path_gen));
      let mut val_cols = MVM::new();
      for (col_name, col_type) in &self.val_cols {
        val_cols.write(col_name, Some(col_type.clone()), commit_timestamp.clone());
      }
      let table_schema = TableSchema { key_cols: self.key_cols.clone(), val_cols };
      gossip.db_schema.insert(table_path_gen.clone(), table_schema);

      // Update `sharding_config`.
      let table_path_full_gen = (self.table_path.clone(), full_gen);
      let mut stripped_shards = Vec::<(TabletKeyRange, TabletGroupId)>::new();
      for (key_range, tid, _) in &self.shards {
        stripped_shards.push((key_range.clone(), tid.clone()));
      }
      gossip.sharding_config.insert(table_path_full_gen, stripped_shards);

      // Update `tablet_address_config`.
      for (_, tid, sid) in &self.shards {
        gossip.tablet_address_config.insert(tid.clone(), sid.clone());
      }

      commit_timestamp
    })
  }
}

impl STMPaxos2PCTMInner<CreateTableTMPayloadTypes> for CreateTableTMInner {
  fn new_follower<IO: BasicIOCtx>(
    _: &mut MasterContext,
    _: &mut IO,
    payload: CreateTableTMPrepared,
  ) -> CreateTableTMInner {
    CreateTableTMInner {
      response_data: None,
      table_path: payload.table_path,
      key_cols: payload.key_cols,
      val_cols: payload.val_cols,
      shards: payload.shards,
      did_commit: false,
    }
  }

  fn mk_prepared_plm<IO: BasicIOCtx>(
    &mut self,
    _: &mut MasterContext,
    _: &mut IO,
  ) -> CreateTableTMPrepared {
    CreateTableTMPrepared {
      table_path: self.table_path.clone(),
      key_cols: self.key_cols.clone(),
      val_cols: self.val_cols.clone(),
      shards: self.shards.clone(),
    }
  }

  fn prepared_plm_inserted<IO: BasicIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    _: &mut IO,
  ) -> BTreeMap<SlaveGroupId, CreateTablePrepare> {
    // The RMs are just the shards. Each shard should be in its own Slave.
    let mut prepares = BTreeMap::<SlaveGroupId, CreateTablePrepare>::new();
    let gossip_view = ctx.gossip.get();
    let gen = next_gen(gossip_view.table_generation.get_last_present_version(&self.table_path));
    for (key_range, tid, sid) in &self.shards {
      prepares.insert(
        sid.clone(),
        CreateTablePrepare {
          tablet_group_id: tid.clone(),
          table_path: self.table_path.clone(),
          gen: gen.clone(),
          key_range: key_range.clone(),
          key_cols: self.key_cols.clone(),
          val_cols: self.val_cols.clone(),
        },
      );
    }
    debug_assert_eq!(prepares.len(), self.shards.len());
    prepares
  }

  fn mk_committed_plm<IO: BasicIOCtx>(
    &mut self,
    _: &mut MasterContext,
    _: &mut IO,
    _: &BTreeMap<SlaveGroupId, CreateTablePrepared>,
  ) -> CreateTableTMCommitted {
    CreateTableTMCommitted {}
  }

  fn committed_plm_inserted<IO: BasicIOCtx>(
    &mut self,
    _: &mut MasterContext,
    _: &mut IO,
    _: &TMCommittedPLm<CreateTableTMPayloadTypes>,
  ) -> BTreeMap<SlaveGroupId, CreateTableCommit> {
    self.did_commit = true;

    // The RMs are just the shards. Each shard should be in its own Slave.
    let mut commits = BTreeMap::<SlaveGroupId, CreateTableCommit>::new();
    for (_, _, sid) in &self.shards {
      commits.insert(sid.clone(), CreateTableCommit {});
    }
    debug_assert_eq!(commits.len(), self.shards.len());
    commits
  }

  fn mk_aborted_plm<IO: BasicIOCtx>(
    &mut self,
    _: &mut MasterContext,
    _: &mut IO,
  ) -> CreateTableTMAborted {
    CreateTableTMAborted {}
  }

  fn aborted_plm_inserted<IO: BasicIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
  ) -> BTreeMap<SlaveGroupId, CreateTableAbort> {
    self.did_commit = false;

    // Potentially respond to the External if we are the leader.
    if ctx.is_leader() {
      if let Some(response_data) = &self.response_data {
        ctx.external_request_id_map.remove(&response_data.request_id);
        io_ctx.send(
          &response_data.sender_eid,
          msg::NetworkMessage::External(msg::ExternalMessage::ExternalDDLQueryAborted(
            msg::ExternalDDLQueryAborted {
              request_id: response_data.request_id.clone(),
              payload: msg::ExternalDDLQueryAbortData::Unknown,
            },
          )),
        );
        self.response_data = None;
      }
    }

    let mut aborts = BTreeMap::<SlaveGroupId, CreateTableAbort>::new();
    for (_, _, sid) in &self.shards {
      aborts.insert(sid.clone(), CreateTableAbort {});
    }
    aborts
  }

  fn mk_closed_plm<IO: BasicIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
  ) -> CreateTableTMClosed {
    let timestamp_hint = if self.did_commit {
      Some(cur_timestamp(io_ctx, ctx.master_config.timestamp_suffix_divisor))
    } else {
      None
    };
    CreateTableTMClosed { timestamp_hint }
  }

  fn closed_plm_inserted<IO: BasicIOCtx>(
    &mut self,
    ctx: &mut MasterContext,
    io_ctx: &mut IO,
    closed_plm: &TMClosedPLm<CreateTableTMPayloadTypes>,
  ) {
    if let Some(timestamp_hint) = &closed_plm.payload.timestamp_hint {
      // This means that the closed_plm is a result of committing the CreateTable.
      let commit_timestamp = self.apply_create(ctx, io_ctx, timestamp_hint.clone());

      // Potentially respond to the External if we are the leader.
      // Note: Recall we will already have responded if the CreateTable had failed.
      if ctx.is_leader() {
        if let Some(response_data) = &self.response_data {
          // This means this is the original Leader that got the query.
          ctx.external_request_id_map.remove(&response_data.request_id);
          io_ctx.send(
            &response_data.sender_eid,
            msg::NetworkMessage::External(msg::ExternalMessage::ExternalDDLQuerySuccess(
              msg::ExternalDDLQuerySuccess {
                request_id: response_data.request_id.clone(),
                timestamp: commit_timestamp.clone(),
              },
            )),
          );
          self.response_data = None;
        }
      }

      // Trace this commit.
      io_ctx.general_trace(GeneralTraceMessage::CommittedQueryId(
        closed_plm.query_id.clone(),
        commit_timestamp.clone(),
      ));

      // Send out GossipData to all Slaves.
      ctx.broadcast_gossip(io_ctx);
    }
  }

  fn leader_changed<IO: BasicIOCtx>(&mut self, _: &mut MasterContext, _: &mut IO) {
    self.response_data = None;
  }

  fn reconfig_snapshot(&self) -> CreateTableTMInner {
    CreateTableTMInner {
      response_data: None,
      table_path: self.table_path.clone(),
      key_cols: self.key_cols.clone(),
      val_cols: self.val_cols.clone(),
      shards: self.shards.clone(),
      did_commit: self.did_commit.clone(),
    }
  }
}

/// Compute the next generating, taking it as 0 if it does not exist yet.
fn next_gen(m_cur_full_gen: Option<&FullGen>) -> Gen {
  if let Some((gen, _)) = m_cur_full_gen {
    gen.next()
  } else {
    Gen(0)
  }
}
