use crate::col_usage::{iterate_stage_ms_query, GeneralStage};
use crate::common::{lookup, TableSchema};
use crate::model::common::TablePath;
use crate::model::common::{proc, ColName, Gen, TierMap, Timestamp, TransTableName};
use crate::multiversion_map::MVM;
use std::collections::{BTreeMap, BTreeSet};

/// Gather every reference to a `TablePath` found in the `query`.
pub fn collect_table_paths(query: &proc::MSQuery) -> BTreeSet<TablePath> {
  let mut table_paths = BTreeSet::<TablePath>::new();
  iterate_stage_ms_query(
    &mut |stage: GeneralStage| match stage {
      GeneralStage::SuperSimpleSelect(query) => {
        if let proc::TableRef::TablePath(table_path) = &query.from {
          table_paths.insert(table_path.clone());
        }
      }
      GeneralStage::Update(query) => {
        table_paths.insert(query.table.clone());
      }
      GeneralStage::Insert(query) => {
        table_paths.insert(query.table.clone());
      }
    },
    query,
  );
  table_paths
}

/// Compute the all TierMaps for the `MSQueryES`.
///
/// The Tier should be where every Read query should be reading from, except
/// if the current stage is an Update, which should be one Tier ahead (i.e.
/// lower) for that TablePath.
pub fn compute_all_tier_maps(ms_query: &proc::MSQuery) -> BTreeMap<TransTableName, TierMap> {
  let mut all_tier_maps = BTreeMap::<TransTableName, TierMap>::new();
  let mut cur_tier_map = BTreeMap::<TablePath, u32>::new();
  for (_, stage) in &ms_query.trans_tables {
    match stage {
      proc::MSQueryStage::SuperSimpleSelect(_) => {}
      proc::MSQueryStage::Update(update) => {
        cur_tier_map.insert(update.table.clone(), 0);
      }
      proc::MSQueryStage::Insert(insert) => {
        cur_tier_map.insert(insert.table.clone(), 0);
      }
    }
  }
  for (trans_table_name, stage) in ms_query.trans_tables.iter().rev() {
    all_tier_maps.insert(trans_table_name.clone(), TierMap { map: cur_tier_map.clone() });
    match stage {
      proc::MSQueryStage::SuperSimpleSelect(_) => {}
      proc::MSQueryStage::Update(update) => {
        *cur_tier_map.get_mut(&update.table).unwrap() += 1;
      }
      proc::MSQueryStage::Insert(insert) => {
        *cur_tier_map.get_mut(&insert.table).unwrap() += 1;
      }
    }
  }
  all_tier_maps
}

/// Computes `extra_req_cols`, which is a class of columns that must be present in the
/// Tablets according to the MSQuery. The presence of these columns need to be validated before
/// other algorithms can run, e.g. `ColUsagePlanner`.
pub fn compute_extra_req_cols(ms_query: &proc::MSQuery) -> BTreeMap<TablePath, Vec<ColName>> {
  let mut extra_req_cols = BTreeMap::<TablePath, Vec<ColName>>::new();

  // Helper to add extra columns to `extra_req_cols` which avoids duplicating
  // ColNames that are already present.
  fn add_cols(
    extra_req_cols: &mut BTreeMap<TablePath, Vec<ColName>>,
    table_path: &TablePath,
    col_names: Vec<ColName>,
  ) {
    // Recall there might already be required columns for this TablePath.
    if !extra_req_cols.contains_key(table_path) {
      extra_req_cols.insert(table_path.clone(), Vec::new());
    }
    let req_cols = extra_req_cols.get_mut(table_path).unwrap();
    for col_name in col_names {
      if !req_cols.contains(&col_name) {
        req_cols.push(col_name);
      }
    }
  }

  iterate_stage_ms_query(
    &mut |stage: GeneralStage| match stage {
      GeneralStage::SuperSimpleSelect(query) => {
        if let proc::TableRef::TablePath(table_path) = &query.from {
          add_cols(&mut extra_req_cols, table_path, query.projection.clone());
        }
      }
      GeneralStage::Update(query) => {
        add_cols(
          &mut extra_req_cols,
          &query.table,
          query.assignment.iter().map(|(c, _)| c).cloned().collect(),
        );
      }
      GeneralStage::Insert(query) => {
        add_cols(&mut extra_req_cols, &query.table, query.columns.clone());
      }
    },
    ms_query,
  );

  extra_req_cols
}

/// Compute miscellaneous data related to QueryPlanning. This can be shared between
/// the Master and MSCoordESs.
pub fn compute_query_plan_data(
  ms_query: &proc::MSQuery,
  table_generation: &MVM<TablePath, Gen>,
  timestamp: Timestamp,
) -> (BTreeMap<TablePath, Gen>, BTreeMap<TablePath, Vec<ColName>>) {
  let mut table_location_map = BTreeMap::<TablePath, Gen>::new();
  iterate_stage_ms_query(
    &mut |stage: GeneralStage| match stage {
      GeneralStage::SuperSimpleSelect(query) => {
        if let proc::TableRef::TablePath(table_path) = &query.from {
          let gen = table_generation.static_read(table_path, timestamp).unwrap();
          table_location_map.insert(table_path.clone(), gen.clone());
        }
      }
      GeneralStage::Update(query) => {
        let gen = table_generation.static_read(&query.table, timestamp).unwrap();
        table_location_map.insert(query.table.clone(), gen.clone());
      }
      GeneralStage::Insert(query) => {
        let gen = table_generation.static_read(&query.table, timestamp).unwrap();
        table_location_map.insert(query.table.clone(), gen.clone());
      }
    },
    ms_query,
  );

  (table_location_map, compute_extra_req_cols(ms_query))
}

pub enum KeyValidationError {
  InvalidUpdate,
  InvalidInsert,
}

/// This function performs validations that include checks on the shape of the query,
/// and checks related to the Key Columns of the Tablets.
///
/// Preconditions:
///   1. All `TablePaths` that appear in `ms_query` must be present in `table_generation`.
///      at `timestamp` (by `static_read`).
///   2. All `(TablePath, Gen)` pairs in `table_generation` must be a key in `db_schema`.
pub fn perform_static_validations(
  ms_query: &proc::MSQuery,
  table_generation: &MVM<TablePath, Gen>,
  db_schema: &BTreeMap<(TablePath, Gen), TableSchema>,
  timestamp: Timestamp,
) -> Result<(), KeyValidationError> {
  // We do some validations of the Update queries:
  //   1. We check that it is not trying to modify a Key Column.
  for (_, stage) in &ms_query.trans_tables {
    match stage {
      proc::MSQueryStage::SuperSimpleSelect(_) => {}
      proc::MSQueryStage::Update(query) => {
        // The TablePath exists, from the above.
        let gen = table_generation.static_read(&query.table, timestamp).unwrap();
        let schema = db_schema.get(&(query.table.clone(), gen.clone())).unwrap();
        for (col_name, _) in &query.assignment {
          if lookup(&schema.key_cols, col_name).is_some() {
            return Err(KeyValidationError::InvalidUpdate);
          }
        }
      }
      proc::MSQueryStage::Insert(_) => {}
    }
  }

  // We do some validations of the Insert queries:
  //   1. We check that all Key Columns are present in `columns`
  //   2. We check that every row in `values` has equal length to `columns`.
  for (_, stage) in &ms_query.trans_tables {
    match stage {
      proc::MSQueryStage::SuperSimpleSelect(_) => {}
      proc::MSQueryStage::Update(_) => {}
      proc::MSQueryStage::Insert(query) => {
        // The TablePath exists, from the above.
        let gen = table_generation.static_read(&query.table, timestamp).unwrap();
        let schema = db_schema.get(&(query.table.clone(), gen.clone())).unwrap();
        // Check that all KeyCols are present
        for (col_name, _) in &schema.key_cols {
          if !query.columns.contains(col_name) {
            return Err(KeyValidationError::InvalidInsert);
          }
        }
        // Check that `values` is valid
        for row in &query.values {
          if row.len() != query.columns.len() {
            return Err(KeyValidationError::InvalidInsert);
          }
        }
      }
    }
  }

  Ok(())
}