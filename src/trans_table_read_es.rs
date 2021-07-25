use crate::col_usage::{
  collect_select_subqueries, collect_top_level_cols, nodes_external_cols,
  nodes_external_trans_tables, ColUsagePlanner,
};
use crate::common::{lookup_pos, mk_qid, IOTypes, NetworkOut, OrigP, QueryESResult, QueryPlan};
use crate::expression::{is_true, EvalError};
use crate::gr_query_es::{GRExecutionS, GRQueryConstructorView, GRQueryES, GRQueryPlan};
use crate::model::common::{
  proc, ColName, ColType, ColValN, ContextRow, ContextSchema, Gen, TableView, Timestamp,
  TransTableName,
};
use crate::model::common::{Context, QueryId, QueryPath, TierMap, TransTableLocationPrefix};
use crate::model::message as msg;
use crate::query_replanning_es::QueryReplanningSqlView;
use crate::server::{
  evaluate_super_simple_select, mk_eval_error, CommonQuery, ContextConstructor, LocalTable,
  ServerContext,
};
use crate::tablet::{
  compute_contexts, Executing, SingleSubqueryStatus, SubqueryFinished, SubqueryPending,
};
use crate::trans_table_read_es::TransTableAction::Wait;
use std::collections::{HashMap, HashSet};
use std::iter::FromIterator;
use std::ops::Deref;
use std::rc::Rc;

pub trait TransTableSource {
  fn get_instance(&self, prefix: &TransTableName, idx: usize) -> &TableView;
  fn get_schema(&self, prefix: &TransTableName) -> Vec<ColName>;
}

#[derive(Debug)]
pub enum TransExecutionS {
  Start,
  Executing(Executing),
  Done,
}

#[derive(Debug)]
pub struct TransTableReadES {
  pub root_query_path: QueryPath,
  pub tier_map: TierMap,
  pub location_prefix: TransTableLocationPrefix,
  pub context: Rc<Context>,

  // Fields needed for responding.
  pub sender_path: QueryPath,
  pub query_id: QueryId,

  // Query-related fields.
  pub sql_query: proc::SuperSimpleSelect,
  pub query_plan: QueryPlan,

  // Dynamically evolving fields.
  pub new_rms: HashSet<QueryPath>,
  pub state: TransExecutionS,

  // Convenience fields
  pub timestamp: Timestamp, // The timestamp read from the GRQueryES
}

pub enum TransTableAction {
  /// This tells the parent Server to wait.
  Wait,
  /// This tells the parent Server to perform subqueries.
  SendSubqueries(Vec<GRQueryES>),
  /// Indicates the ES succeeded with the given result.
  Success(QueryESResult),
  /// Indicates the ES failed with a QueryError.
  QueryError(msg::QueryError),
  /// Indicates the ES failed with insufficient columns in the Context.
  ColumnsDNE(Vec<ColName>),
}

#[derive(Debug)]
pub enum FullTransTableReadES {
  QueryReplanning(TransQueryReplanningES),
  Executing(TransTableReadES),
}

// -----------------------------------------------------------------------------------------------
//  LocalTable
// -----------------------------------------------------------------------------------------------

struct TransLocalTable<'a, SourceT: TransTableSource> {
  /// The TransTableSource the ES is operating on.
  trans_table_source: &'a SourceT,
  trans_table_name: &'a TransTableName,
  /// The schema read TransTable.
  schema: Vec<ColName>,
}

impl<'a, SourceT: TransTableSource> TransLocalTable<'a, SourceT> {
  fn new(
    trans_table_source: &'a SourceT,
    trans_table_name: &'a TransTableName,
  ) -> TransLocalTable<'a, SourceT> {
    TransLocalTable {
      trans_table_source,
      trans_table_name,
      schema: trans_table_source.get_schema(trans_table_name),
    }
  }
}

impl<'a, SourceT: TransTableSource> LocalTable for TransLocalTable<'a, SourceT> {
  fn contains_col(&self, col: &ColName) -> bool {
    self.schema.contains(col)
  }

  fn get_rows(
    &self,
    parent_context_schema: &ContextSchema,
    parent_context_row: &ContextRow,
    col_names: &Vec<ColName>,
  ) -> Result<Vec<(Vec<ColValN>, u64)>, EvalError> {
    // First, we look up the TransTableInstance
    let trans_table_name_pos = parent_context_schema
      .trans_table_context_schema
      .iter()
      .position(|prefix| &prefix.trans_table_name == self.trans_table_name)
      .unwrap();
    let trans_table_instance_pos =
      parent_context_row.trans_table_context_row.get(trans_table_name_pos).unwrap();
    let trans_table_instance =
      self.trans_table_source.get_instance(&self.trans_table_name, *trans_table_instance_pos);

    // Next, we select the desired columns and compress them before returning it.
    let mut sub_view = TableView::new(col_names.clone());
    for (row, count) in &trans_table_instance.rows {
      let mut new_row = Vec::<ColValN>::new();
      for col in col_names {
        let pos = trans_table_instance.col_names.iter().position(|cur_col| cur_col == col).unwrap();
        new_row.push(row.get(pos).unwrap().clone());
      }
      sub_view.add_row_multi(new_row, *count);
    }

    Ok(sub_view.rows.into_iter().collect())
  }
}

// -----------------------------------------------------------------------------------------------
//  Implementation
// -----------------------------------------------------------------------------------------------

impl FullTransTableReadES {
  pub fn start<T: IOTypes, SourceT: TransTableSource>(
    &mut self,
    ctx: &mut ServerContext<T>,
    trans_table_source: &SourceT,
  ) -> TransTableAction {
    let plan_es = cast!(Self::QueryReplanning, self).unwrap();
    let action = plan_es.start::<T, SourceT>(ctx, trans_table_source);
    self.handle_replanning_action(ctx, trans_table_source, action)
  }

  /// Handle Master Response, routing it to the QueryReplanning.
  pub fn handle_master_response<T: IOTypes, SourceT: TransTableSource>(
    &mut self,
    ctx: &mut ServerContext<T>,
    trans_table_source: &SourceT,
    gossip_gen: Gen,
    tree: msg::FrozenColUsageTree,
  ) -> TransTableAction {
    let plan_es = cast!(FullTransTableReadES::QueryReplanning, self).unwrap();
    let action = plan_es.handle_master_response(ctx, gossip_gen, tree);
    self.handle_replanning_action(ctx, trans_table_source, action)
  }

  /// Handle the `action` sent back by the `TransQueryReplanningES`.
  fn handle_replanning_action<T: IOTypes, SourceT: TransTableSource>(
    &mut self,
    ctx: &mut ServerContext<T>,
    trans_table_source: &SourceT,
    action: TransQueryReplanningAction,
  ) -> TransTableAction {
    match action {
      TransQueryReplanningAction::Wait => TransTableAction::Wait,
      TransQueryReplanningAction::Success => {
        // If the QueryReplanning was successful, we move the FullTransTableReadES
        // to Executing in the Start state, and immediately start executing it.
        let plan_es = cast!(FullTransTableReadES::QueryReplanning, self).unwrap();
        *self = FullTransTableReadES::Executing(TransTableReadES {
          root_query_path: plan_es.root_query_path.clone(),
          tier_map: plan_es.tier_map.clone(),
          location_prefix: plan_es.location_prefix.clone(),
          context: plan_es.context.clone(),
          sender_path: plan_es.sender_path.clone(),
          query_id: plan_es.query_id.clone(),
          sql_query: plan_es.sql_query.clone(),
          query_plan: plan_es.query_plan.clone(),
          new_rms: Default::default(),
          state: TransExecutionS::Start,
          timestamp: plan_es.timestamp,
        });
        self.start_trans_table_read_es(ctx, trans_table_source)
      }
      TransQueryReplanningAction::QueryError(query_error) => {
        // Forward up the QueryError. We do not need to ECU because
        // QueryReplanning will be in Done
        TransTableAction::QueryError(query_error)
      }
      TransQueryReplanningAction::ColumnsDNE(missing_cols) => {
        // Forward up the QueryError. We do not need to ECU because
        // QueryReplanning will be in Done
        TransTableAction::ColumnsDNE(missing_cols)
      }
    }
  }

  /// Constructs and returns subqueries.
  fn start_trans_table_read_es<T: IOTypes, SourceT: TransTableSource>(
    &mut self,
    ctx: &mut ServerContext<T>,
    trans_table_source: &SourceT,
  ) -> TransTableAction {
    let es = cast!(Self::Executing, self).unwrap();
    assert!(matches!(&es.state, &TransExecutionS::Start));
    // Here, we first construct all of the subquery Contexts using the
    // ContextConstructor, and then we construct GRQueryESs.

    // Compute children.
    let mut children = Vec::<(Vec<ColName>, Vec<TransTableName>)>::new();
    for child in &es.query_plan.col_usage_node.children {
      children.push((nodes_external_cols(child), nodes_external_trans_tables(child)));
    }

    // Create the child context. Recall that we are able to unwrap `compute_contexts`
    // for the case TransTables since there is no KeyBound Computation.
    let trans_table_name = &es.location_prefix.trans_table_name;
    let local_table = TransLocalTable::new(trans_table_source, trans_table_name);
    let child_contexts = compute_contexts(es.context.deref(), local_table, children).unwrap();

    // Finally, compute the GRQueryESs.
    let subquery_view = GRQueryConstructorView {
      root_query_path: &es.root_query_path,
      tier_map: &es.tier_map,
      timestamp: &es.timestamp,
      sql_query: &es.sql_query,
      query_plan: &es.query_plan,
      query_id: &es.query_id,
      context: &es.context,
    };
    let mut gr_query_ess = Vec::<GRQueryES>::new();
    for (subquery_idx, child_context) in child_contexts.into_iter().enumerate() {
      gr_query_ess.push(subquery_view.mk_gr_query_es(
        mk_qid(&mut ctx.rand),
        Rc::new(child_context),
        subquery_idx,
      ));
    }

    if gr_query_ess.is_empty() {
      // Since there are no subqueries, we can go straight to finishing the ES.
      self.finish_trans_table_read_es(ctx, trans_table_source)
    } else {
      // Here, we have computed all GRQueryESs, and we can now add them to
      // `subquery_status`.
      let mut subqueries = Vec::<SingleSubqueryStatus>::new();
      for gr_query_es in &gr_query_ess {
        subqueries.push(SingleSubqueryStatus::Pending(SubqueryPending {
          context: gr_query_es.context.clone(),
          query_id: gr_query_es.query_id.clone(),
        }));
      }

      // Move the ES to the Executing state.
      es.state = TransExecutionS::Executing(Executing {
        completed: 0,
        subqueries,
        row_region: vec![], // This doesn't make sense for TransTables...
      });

      // Return the subqueries so that they can be driven from the parent Server.
      TransTableAction::SendSubqueries(gr_query_ess)
    }
  }

  /// Handles InternalColumnsDNE
  pub fn handle_internal_columns_dne<T: IOTypes, SourceT: TransTableSource>(
    &mut self,
    ctx: &mut ServerContext<T>,
    trans_table_source: &SourceT,
    subquery_id: QueryId,
    rem_cols: Vec<ColName>,
  ) -> TransTableAction {
    let es = cast!(Self::Executing, self).unwrap();
    let executing = cast!(TransExecutionS::Executing, &mut es.state).unwrap();
    let trans_table_name = &es.location_prefix.trans_table_name;
    let trans_table_schema = trans_table_source.get_schema(trans_table_name);

    // First, we check if all columns in `rem_cols` are at least present in the context.
    let mut missing_cols = Vec::<ColName>::new();
    for col in &rem_cols {
      if !trans_table_schema.contains(col) {
        if !es.context.context_schema.column_context_schema.contains(col) {
          missing_cols.push(col.clone());
        }
      }
    }

    if !missing_cols.is_empty() {
      // If there are missing columns, we ECU and propagate up the ColumnsDNE.
      self.exit_and_clean_up(ctx);
      TransTableAction::ColumnsDNE(missing_cols)
    } else {
      // This means there are no missing columns, and so we can try the
      // GRQueryES again by extending its Context.

      // Find the subquery that just aborted. There should always be such a Subquery.
      let subquery_idx = executing.find_subquery(&subquery_id).unwrap();
      let single_status = executing.subqueries.get_mut(subquery_idx).unwrap();
      let pending_status = cast!(SingleSubqueryStatus::Pending, single_status).unwrap();
      let context_schema = &pending_status.context.context_schema;

      // Create the child context.
      let mut new_columns = context_schema.column_context_schema.clone();
      new_columns.extend(rem_cols);
      let children = vec![(new_columns, context_schema.trans_table_names())];
      let local_table = TransLocalTable::new(trans_table_source, trans_table_name);
      let child_contexts = compute_contexts(es.context.deref(), local_table, children).unwrap();
      assert_eq!(child_contexts.len(), 1);
      let context = Rc::new(child_contexts.into_iter().next().unwrap());

      // Construct the GRQueryES
      let subquery_view = GRQueryConstructorView {
        root_query_path: &es.root_query_path,
        tier_map: &es.tier_map,
        timestamp: &es.timestamp,
        sql_query: &es.sql_query,
        query_plan: &es.query_plan,
        query_id: &es.query_id,
        context: &es.context,
      };
      let gr_query_id = mk_qid(ctx.rand);
      let gr_query_es =
        subquery_view.mk_gr_query_es(gr_query_id.clone(), context.clone(), subquery_idx);

      // Update the `executing` state to contain the new subquery_id.
      *single_status =
        SingleSubqueryStatus::Pending(SubqueryPending { query_id: gr_query_id, context });

      // Return the GRQueryESs for execution.
      TransTableAction::SendSubqueries(vec![gr_query_es])
    }
  }

  /// This is can be called both for if a subquery fails, or if there is a LateralError
  /// due to the ES owning the TransTable disappears. This simply responds to the sender
  /// and Exits and Clean Ups this ES.
  pub fn handle_internal_query_error<T: IOTypes>(
    &mut self,
    ctx: &mut ServerContext<T>,
    query_error: msg::QueryError,
  ) -> TransTableAction {
    self.exit_and_clean_up(ctx);
    TransTableAction::QueryError(query_error)
  }

  /// Handles a Subquery completing
  pub fn handle_subquery_done<T: IOTypes, SourceT: TransTableSource>(
    &mut self,
    ctx: &mut ServerContext<T>,
    trans_table_source: &SourceT,
    subquery_id: QueryId,
    subquery_new_rms: HashSet<QueryPath>,
    (_, table_views): (Vec<ColName>, Vec<TableView>),
  ) -> TransTableAction {
    let es = cast!(Self::Executing, self).unwrap();

    // Add the subquery results into the TableReadES.
    es.new_rms.extend(subquery_new_rms);
    let executing_state = cast!(TransExecutionS::Executing, &mut es.state).unwrap();
    let subquery_idx = executing_state.find_subquery(&subquery_id).unwrap();
    let single_status = executing_state.subqueries.get_mut(subquery_idx).unwrap();
    let context = &cast!(SingleSubqueryStatus::Pending, single_status).unwrap().context.clone();
    *single_status = SingleSubqueryStatus::Finished(SubqueryFinished {
      context: context.clone(),
      result: table_views,
    });
    executing_state.completed += 1;

    // If all subqueries have been evaluated, finish the TransTableReadES
    // and respond to the client.
    let num_subqueries = executing_state.subqueries.len();
    if executing_state.completed < num_subqueries {
      TransTableAction::Wait
    } else {
      self.finish_trans_table_read_es(ctx, trans_table_source)
    }
  }

  /// Handles a ES finishing with all subqueries results in.
  fn finish_trans_table_read_es<T: IOTypes, SourceT: TransTableSource>(
    &mut self,
    _: &mut ServerContext<T>,
    trans_table_source: &SourceT,
  ) -> TransTableAction {
    let es = cast!(Self::Executing, self).unwrap();
    let executing_state = cast!(TransExecutionS::Executing, &mut es.state).unwrap();

    // Compute children.
    let mut children = Vec::<(Vec<ColName>, Vec<TransTableName>)>::new();
    let mut subquery_results = Vec::<Vec<TableView>>::new();
    for single_status in &executing_state.subqueries {
      let result = cast!(SingleSubqueryStatus::Finished, single_status).unwrap();
      let context_schema = &result.context.context_schema;
      children
        .push((context_schema.column_context_schema.clone(), context_schema.trans_table_names()));
      subquery_results.push(result.result.clone());
    }

    // Create the ContextConstructor.
    let context_constructor = ContextConstructor::new(
      es.context.context_schema.clone(),
      TransLocalTable::new(trans_table_source, &es.location_prefix.trans_table_name),
      children,
    );

    // These are all of the `ColNames` we need in order to evaluate the Select.
    let mut top_level_cols_set = HashSet::<ColName>::new();
    top_level_cols_set.extend(collect_top_level_cols(&es.sql_query.selection));
    top_level_cols_set.extend(es.sql_query.projection.clone());
    let top_level_col_names = Vec::from_iter(top_level_cols_set.into_iter());

    // Finally, iterate over the Context Rows of the subqueries and compute the final values.
    let mut res_table_views = Vec::<TableView>::new();
    for _ in 0..es.context.context_rows.len() {
      res_table_views.push(TableView::new(es.sql_query.projection.clone()));
    }

    let eval_res = context_constructor.run(
      &es.context.context_rows,
      top_level_col_names.clone(),
      &mut |context_row_idx: usize,
            top_level_col_vals: Vec<ColValN>,
            contexts: Vec<(ContextRow, usize)>,
            count: u64| {
        // First, we extract the subquery values using the child Context indices.
        let mut subquery_vals = Vec::<TableView>::new();
        for (subquery_idx, (_, child_context_idx)) in contexts.iter().enumerate() {
          let val = subquery_results.get(subquery_idx).unwrap().get(*child_context_idx).unwrap();
          subquery_vals.push(val.clone());
        }

        // Now, we evaluate all expressions in the SQL query and amend the
        // result to this TableView (if the WHERE clause evaluates to true).
        let evaluated_select = evaluate_super_simple_select(
          &es.sql_query,
          &top_level_col_names,
          &top_level_col_vals,
          &subquery_vals,
        )?;
        if is_true(&evaluated_select.selection)? {
          // This means that the current row should be selected for the result. Thus, we take
          // the values of the project columns and insert it into the appropriate TableView.
          let mut res_row = Vec::<ColValN>::new();
          for res_col_name in &es.sql_query.projection {
            let idx = top_level_col_names.iter().position(|k| res_col_name == k).unwrap();
            res_row.push(top_level_col_vals.get(idx).unwrap().clone());
          }

          res_table_views[context_row_idx].add_row_multi(res_row, count);
        };
        Ok(())
      },
    );

    if let Err(eval_error) = eval_res {
      es.state = TransExecutionS::Done;
      return TransTableAction::QueryError(mk_eval_error(eval_error));
    }

    // Signal Success and return the data.
    es.state = TransExecutionS::Done;
    TransTableAction::Success(QueryESResult {
      result: (es.sql_query.projection.clone(), res_table_views),
      new_rms: es.new_rms.iter().cloned().collect(),
    })
  }

  /// Cleans up all currently owned resources, and goes to Done.
  pub fn exit_and_clean_up<T: IOTypes>(&mut self, ctx: &mut ServerContext<T>) {
    match self {
      FullTransTableReadES::QueryReplanning(es) => es.exit_and_clean_up(ctx),
      FullTransTableReadES::Executing(es) => {
        match &es.state {
          TransExecutionS::Start => {}
          TransExecutionS::Executing(executing) => {
            // Here, we need to cancel every Subquery.
            for single_status in &executing.subqueries {
              match single_status {
                SingleSubqueryStatus::LockingSchemas(_) => panic!(),
                SingleSubqueryStatus::PendingReadRegion(_) => panic!(),
                SingleSubqueryStatus::Pending(_) => {}
                SingleSubqueryStatus::Finished(_) => {}
              }
            }
          }
          TransExecutionS::Done => {}
        }
        es.state = TransExecutionS::Done
      }
    }
  }

  /// Get the `TransTableLocationPrefix` of this ES.
  pub fn location_prefix(&self) -> &TransTableLocationPrefix {
    match self {
      FullTransTableReadES::QueryReplanning(es) => &es.location_prefix,
      FullTransTableReadES::Executing(es) => &es.location_prefix,
    }
  }

  /// Get the `QueryPath` of the sender of this ES.
  pub fn sender_path(&self) -> QueryPath {
    match &self {
      FullTransTableReadES::QueryReplanning(es) => es.sender_path.clone(),
      FullTransTableReadES::Executing(es) => es.sender_path.clone(),
    }
  }

  /// Get the `QueryId` of the sender of this ES.
  pub fn query_id(&self) -> QueryId {
    match &self {
      FullTransTableReadES::QueryReplanning(es) => es.query_id.clone(),
      FullTransTableReadES::Executing(es) => es.query_id.clone(),
    }
  }
}

// -----------------------------------------------------------------------------------------------
//  TransQueryReplanningES
// -----------------------------------------------------------------------------------------------

#[derive(Debug)]
pub enum TransQueryReplanningS {
  Start,
  /// Used to wait on the master
  MasterQueryReplanning {
    master_query_id: QueryId,
  },
  Done,
}

#[derive(Debug)]
pub struct TransQueryReplanningES {
  /// The below fields are from PerformQuery and are passed through to TableReadES.
  pub root_query_path: QueryPath,
  pub tier_map: TierMap,
  pub query_id: QueryId,

  // These members are parallel to the messages in `msg::GeneralQuery`.
  pub location_prefix: TransTableLocationPrefix,
  pub context: Rc<Context>,
  pub sql_query: proc::SuperSimpleSelect,
  pub query_plan: QueryPlan,

  /// Path of the original sender (needed for responding with errors).
  pub sender_path: QueryPath,
  /// The OrigP of the Task holding this CommonQueryReplanningES
  pub orig_p: OrigP,
  /// The state of the CommonQueryReplanningES
  pub state: TransQueryReplanningS,

  // Convenience fields
  pub timestamp: Timestamp, // The timestamp read from the GRQueryES
}

pub enum TransQueryReplanningAction {
  /// Indicates the parent needs to wait, making sure to fowards MasterQueryReplanning responses.
  Wait,
  /// Indicates the that TransQueryReplanningES has computed a valid query where the Context
  /// is sufficient, and it's stored in the `query_plan` field.
  Success,
  /// Indicates a valid QueryPlan couldn't be computed and resulted in a QueryError.
  QueryError(msg::QueryError),
  /// Indicates a valid QueryPlan couldn't be computed because there were insufficient columns.
  ColumnsDNE(Vec<ColName>),
}

impl TransQueryReplanningES {
  fn start<T: IOTypes, SourceT: TransTableSource>(
    &mut self,
    ctx: &mut ServerContext<T>,
    trans_table_source: &SourceT,
  ) -> TransQueryReplanningAction {
    matches!(self.state, TransQueryReplanningS::Start);
    // First, verify that the select columns are in the TransTable.
    let schema_cols = trans_table_source.get_schema(&self.location_prefix.trans_table_name);
    for col in &self.sql_query.projection {
      if !schema_cols.contains(col) {
        // This means a Required Column is not present. Thus, we go to Done and return the error.
        self.state = TransQueryReplanningS::Done;
        return TransQueryReplanningAction::QueryError(msg::QueryError::RequiredColumnsDNE {
          msg: String::new(),
        });
      }
    }

    // Next, we see if the ctx has a higher GossipGen. If so, we should compute a
    // new QueryPlan (for system liveness reasons).
    if ctx.gossip.gossip_gen <= self.query_plan.gossip_gen {
      // We should use QueryPlan of the sender, so we return it. (Recall there is no
      // Column Locking, etc, required).
      self.state = TransQueryReplanningS::Done;
      TransQueryReplanningAction::Success
    } else {
      // This means we must recompute the QueryPlan.
      let mut planner = ColUsagePlanner {
        gossiped_db_schema: &ctx.gossip.gossiped_db_schema,
        timestamp: self.timestamp,
      };

      // Recall that `trans_table_schemas` should contain the schema of the `trans_table_source`.
      let col_usage_node = planner.compute_frozen_col_usage_node(
        &mut self.query_plan.trans_table_schemas.clone(),
        &self.sql_query.from,
        &self.sql_query.exprs(),
      );

      // Next, we check to see if all ColNames in `external_cols` is contaiend
      // in the Context. If not, we have to consult the Master.
      for col in &col_usage_node.external_cols {
        if !self.context.context_schema.column_context_schema.contains(&col) {
          // This means we need to consult the Master.
          let master_query_id = mk_qid(ctx.rand);
          ctx.network_output.send(
            &ctx.master_eid,
            msg::NetworkMessage::Master(msg::MasterMessage::PerformMasterFrozenColUsage(
              msg::PerformMasterFrozenColUsage {
                sender_path: ctx.mk_query_path(self.query_id.clone()),
                query_id: master_query_id.clone(),
                timestamp: self.timestamp,
                trans_table_schemas: self.query_plan.trans_table_schemas.clone(),
                col_usage_tree: msg::ColUsageTree::MSQueryStage(self.sql_query.ms_query_stage()),
              },
            )),
          );

          // Advance Replanning State.
          self.state = TransQueryReplanningS::MasterQueryReplanning { master_query_id };
          return TransQueryReplanningAction::Wait;
        }
      }

      // If we make it here, we have a valid QueryPlan and we are done.
      self.query_plan.gossip_gen = ctx.gossip.gossip_gen;
      self.query_plan.col_usage_node = col_usage_node;
      self.state = TransQueryReplanningS::Done;
      TransQueryReplanningAction::Success
    }
  }

  /// Handles the Query Plan constructed by the Master.
  pub fn handle_master_response<T: IOTypes>(
    &mut self,
    _: &mut ServerContext<T>,
    gossip_gen: Gen,
    tree: msg::FrozenColUsageTree,
  ) -> TransQueryReplanningAction {
    // Recall that since we only send single nodes, we expect the `tree` to just be a `node`.
    let (_, col_usage_node) = cast!(msg::FrozenColUsageTree::ColUsageNode, tree).unwrap();

    // Compute the set of External Columns that still aren't in the Context.
    let mut missing_cols = Vec::<ColName>::new();
    for col in &col_usage_node.external_cols {
      if !self.context.context_schema.column_context_schema.contains(&col) {
        missing_cols.push(col.clone());
      }
    }

    if !missing_cols.is_empty() {
      // If the above set is non-empty, that means the QueryReplanning has conclusively
      // detected an insufficient Context, and move to Done and return the missing columns.
      self.state = TransQueryReplanningS::Done;
      TransQueryReplanningAction::ColumnsDNE(missing_cols)
    } else {
      // This means the QueryReplanning was a success, so we update the QueryPlan and go to Done.
      self.query_plan.gossip_gen = gossip_gen;
      self.query_plan.col_usage_node = col_usage_node;
      self.state = TransQueryReplanningS::Done;
      TransQueryReplanningAction::Success
    }
  }

  /// This Exits and Cleans up this QueryReplanningES
  pub fn exit_and_clean_up<T: IOTypes>(&mut self, ctx: &mut ServerContext<T>) {
    match &self.state {
      TransQueryReplanningS::Start => {}
      TransQueryReplanningS::MasterQueryReplanning { master_query_id } => {
        // If the removal was successful, we should also send a Cancellation
        // message to the Master.
        ctx.network_output.send(
          &ctx.master_eid,
          msg::NetworkMessage::Master(msg::MasterMessage::CancelMasterFrozenColUsage(
            msg::CancelMasterFrozenColUsage { query_id: master_query_id.clone() },
          )),
        );
      }
      TransQueryReplanningS::Done => {}
    }
    self.state = TransQueryReplanningS::Done;
  }
}
