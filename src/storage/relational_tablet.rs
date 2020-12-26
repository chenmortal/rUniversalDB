use crate::model::common::{
  ColumnName, ColumnType, ColumnValue, PrimaryKey, Row, Schema, Timestamp,
};
use crate::storage::multiversion_map::MultiVersionMap;

/// Terminology:
///
/// Row Key - The Primary Key value of a Row
/// Row Val - The value of the non-Primary Key columns of a Row
///           (in sorted order according to the schema layout)
/// Value Cell - A value of a non-Primary Key column when a given
///              Row is understood
/// Key Column - A column of a Primary Key column when a given Row
///              is understood
/// Value Column - A column of a non-Primary Key column when a given
///                Row is understood.

/// These are the values that the MVM maps to. The only difference between
/// this an `ColumnValue` is the presence of `Unit`, which is a special value
/// used to indicate that a row itself is present. We don't want to include `Unit`
/// in `ColumnValue` because it's not a SQL type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StorageValue {
  Int(i32),
  Bool(bool),
  String(String),
  Unit,
}

/// We define a convenient conversion function from
/// StorageValue to ColumnValue.
impl StorageValue {
  fn convert(self) -> ColumnValue {
    match self {
      StorageValue::Int(i32) => ColumnValue::Int(i32),
      StorageValue::Bool(bool) => ColumnValue::Bool(bool),
      StorageValue::String(string) => ColumnValue::String(string),
      StorageValue::Unit => panic!("Cannot convert `Unit` StorageValue to ColumnValue."),
    }
  }
}

/// We define a convenient conversion function from
/// ColumnValue to StorageValue.
impl ColumnValue {
  fn convert(self) -> StorageValue {
    match self {
      ColumnValue::Int(i32) => StorageValue::Int(i32),
      ColumnValue::Bool(bool) => StorageValue::Bool(bool),
      ColumnValue::String(string) => StorageValue::String(string),
    }
  }
}

#[derive(Debug)]
pub struct RelationalTablet {
  mvm: MultiVersionMap<(PrimaryKey, Option<ColumnName>), StorageValue>,
  schema: Schema,
}

impl RelationalTablet {
  pub fn new(schema: Schema) -> RelationalTablet {
    RelationalTablet {
      mvm: MultiVersionMap::new(),
      schema,
    }
  }

  pub fn schema(&self) -> &Schema {
    &self.schema
  }

  /// Returns false if the types don't match, and true otherwise.
  fn check_type_match(value: Option<&ColumnValue>, col_type: &ColumnType) -> bool {
    match (value, col_type) {
      (Some(ColumnValue::Int(_)), ColumnType::Int) => true,
      (Some(ColumnValue::String(_)), ColumnType::String) => true,
      (Some(ColumnValue::Bool(_)), ColumnType::Bool) => true,
      (None, _) => true,
      _ => false,
    }
  }

  /// This checks to see if the provided row conforms to the schema. For this
  /// check to pass, the  Types of the elements in `row.key` and `row.value`
  /// must align with the Types of the elements in `schema.key_cols` and
  /// `schema.val_cols`.
  fn verify_row(&self, row: &Row) -> bool {
    return self.verify_row_key(&row.key) && self.verify_row_val(&row.val);
  }

  fn verify_row_key(&self, key: &PrimaryKey) -> bool {
    if key.cols.len() == self.schema.key_cols.len() {
      for (col_val, (col_type, _)) in key.cols.iter().zip(&self.schema.key_cols) {
        if !RelationalTablet::check_type_match(Some(&col_val), col_type) {
          return false;
        }
      }
    }
    return true;
  }

  fn verify_row_val(&self, val: &Vec<Option<ColumnValue>>) -> bool {
    if val.len() == self.schema.val_cols.len() {
      for (col_val, (col_type, _)) in val.iter().zip(&self.schema.val_cols) {
        if !RelationalTablet::check_type_match((&col_val).as_ref(), col_type) {
          return false;
        }
      }
      return true;
    }
    return false;
  }

  /// Inserts the row into the RelationalTablet. This method first checks
  /// to see if the `row`'s key columns and value columns conform to the
  /// schema. Then, it inserts the row into the MultiVersionMap. If either
  /// of these steps fails, we return false, otherwise we return true.
  pub fn insert_row(&mut self, row: &Row, timestamp: Timestamp) -> Result<(), String> {
    if !self.verify_row(row) {
      return Err(String::from(
        "The given row does not conform to the schema.",
      ));
    }

    // If the row isn't present, and we can't make it present because
    // it's lat is too high, then the insertion fails.
    let mvm_key = (row.key.clone(), None);
    if self.mvm.static_read(&mvm_key, timestamp) == None {
      if self.mvm.get_lat(&mvm_key) >= timestamp {
        // This means the row doesn't exist at the timesstamp
        // and the lat is too high to reintroduce it.
        return Err(String::from(
          "The row doesn't exist at the given `timestamp` and can't be re-introduced.",
        ));
      } else {
        // Since the row doesn't exist at `timestamp`, that means
        // neither to any of the value cells. Since we can both introduce
        // the row and write to the value cells, we can perform the insert.
        self
          .mvm
          .write(&mvm_key, Some(StorageValue::Unit), timestamp)
          .unwrap();
      }
    } else {
      // Although the row is present at `timestamp`, we must make sure
      // we can actually write to the value cells.
      for (_, col_name) in &self.schema.val_cols {
        let mvm_key = (row.key.clone(), Some(col_name.clone()));
        if self.mvm.get_lat(&mvm_key) >= timestamp {
          // The lat of one of the value cells is too high.
          return Err(String::from(
            "The row exists, but one of the column's lat is too high.",
          ));
        }
      }
    }

    // At this point, the Row Key will surely be present in `mvm`,
    // and the Row Value will surely be writable.
    let zipped = self.schema.val_cols.iter().zip(&row.val);
    for ((_, col_name), val) in zipped {
      let mvm_key = (row.key.clone(), Some(col_name.clone()));
      self
        .mvm
        .write(&mvm_key, val.clone().map(|v| v.convert()), timestamp)
        .unwrap();
    }

    return Ok(());
  }

  /// This function generally only updates a subset of the value
  /// columns. The other Value columns remain unchanged, including
  /// their `lat`s. The caller of this function must know what
  /// they're doing; trying to write into the past is a fatal error.
  pub fn insert_partial_vals(
    &mut self,
    _key: PrimaryKey,
    _partial_val: Vec<(ColumnName, Option<ColumnValue>)>,
    _timestamp: &Timestamp,
  ) {
    panic!("TODO: implement.")
  }

  /// This too ins a dumb function that doesn't do any schema checks, etc.
  /// It just sees if the `partial_val` is present. If it isn't, then the
  /// `key` is deleted. And if it is, the specific updates in the
  /// `partial_val` is applied to the relational tablet.
  pub fn insert_row_diff(
    &mut self,
    _key: PrimaryKey,
    _partial_val: Option<Vec<(ColumnName, Option<ColumnValue>)>>,
    _timestamp: &Timestamp,
  ) {
    panic!("TODO: implement");
  }

  /// TODO: Write this
  /// Returns if the column name exists in the schema or not.
  pub fn col_name_exists(&self, _val_col: &ColumnName) -> bool {
    panic!("TODO: implement.")
  }

  /// TODO: Write this
  /// Returns if the column name exists in the schema or not.
  pub fn col_names_exists(&self, _val_cols: &Vec<ColumnName>) -> bool {
    panic!("TODO: implement.")
  }

  /// TODO: Write this
  /// Returns if the ColumnValue's type matches that of the ColumnValue
  pub fn type_matches(_col_val: &ColumnValue, _col_type: &ColumnType) -> bool {
    panic!("TODO: implement.")
  }

  /// TODO: Write this
  /// This function generally only updates a subset of the value
  /// columns. The other Value columns remain unchanged, including
  /// their `lat`s.
  pub fn insert_partial_val(
    &mut self,
    _key: PrimaryKey,
    _val_col: ColumnName,
    _val: Option<ColumnValue>,
    _timestamp: &Timestamp,
  ) {
    panic!("TODO: implement.")
  }

  /// TODO: Write this
  /// This is a dumb function. It doesn't check if the ColumnName
  /// is actually part of the schema. It just appends col_name to key,
  /// and does a lookup in the mvm. Thus, whether the ColumnName in
  /// the Schema exists must be checked before.
  pub fn get_partial_val(
    &self,
    _key: &PrimaryKey,
    _col_name: &ColumnName,
    _timestamp: &Timestamp,
  ) -> Option<ColumnValue> {
    panic!("TODO: implement.")
  }

  /// TODO: Write this
  /// This is essentially a snapshot read of all keys at the timestmap given.
  pub fn get_keys(&self, _timestamp: &Timestamp) -> Vec<PrimaryKey> {
    panic!("TODO: implement.")
  }

  /// This function returns an error if the key doesn't conform
  /// to the schema. Otherwise, it returns None if the row doesn't
  /// exist at the timestamp, and the row if it does.
  pub fn read_row(
    &mut self,
    key: &PrimaryKey,
    timestamp: Timestamp,
  ) -> Result<Option<Row>, String> {
    if !self.verify_row_key(key) {
      return Err(String::from(
        "The given key does not confrom to the schema.",
      ));
    }
    if self.mvm.read(&(key.clone(), None), timestamp) == None {
      return Ok(None);
    } else {
      let mut val_col_values = Vec::new();
      for (_, col_name) in &self.schema.val_cols {
        let mvm_key = (key.clone(), Some(col_name.clone()));
        val_col_values.push(self.mvm.read(&mvm_key, timestamp));
      }
      return Ok(Some(Row {
        key: key.clone(),
        val: val_col_values
          .iter()
          .map(|v| v.clone().map(|v| v.convert()))
          .collect(),
      }));
    }
  }
}

#[cfg(test)]
mod tests {
  use crate::model::common::{
    ColumnName as CN, ColumnType as CT, ColumnValue as CV, PrimaryKey, Row, Schema, Timestamp,
  };
  use crate::storage::relational_tablet::RelationalTablet;

  #[test]
  fn single_row_insert_test() {
    let mut tablet = RelationalTablet::new(Schema {
      key_cols: vec![
        (CT::String, CN(String::from("name"))),
        (CT::Int, CN(String::from("age"))),
      ],
      val_cols: vec![
        (CT::String, CN(String::from("email"))),
        (CT::Int, CN(String::from("income"))),
      ],
    });

    let k = PrimaryKey {
      cols: vec![CV::String(String::from("Fred")), CV::Int(20)],
    };
    let v1 = vec![Some(CV::String(String::from("e1"))), None];
    let v2 = vec![Some(CV::String(String::from("e1"))), Some(CV::Int(60_000))];
    let v3 = vec![Some(CV::String(String::from("e2"))), Some(CV::Int(80_000))];

    let row1 = Row {
      key: k.clone(),
      val: v1.clone(),
    };

    let row2 = Row {
      key: k.clone(),
      val: v2.clone(),
    };

    let row3 = Row {
      key: k.clone(),
      val: v2.clone(),
    };

    let row4 = Row {
      key: k.clone(),
      val: v3.clone(),
    };

    assert!(tablet.insert_row(&row1, Timestamp(2)).is_ok());
    assert!(tablet.insert_row(&row2, Timestamp(2)).is_err());
    assert!(tablet.insert_row(&row3, Timestamp(3)).is_ok());
    assert_eq!(tablet.read_row(&k, Timestamp(2)).unwrap().unwrap(), row1);
    assert_eq!(tablet.read_row(&k, Timestamp(4)).unwrap().unwrap(), row3);
    assert!(tablet.insert_row(&row4, Timestamp(4)).is_err());
    assert!(tablet.insert_row(&row4, Timestamp(5)).is_ok());
    assert_eq!(tablet.read_row(&k, Timestamp(6)).unwrap().unwrap(), row4);
  }
}
