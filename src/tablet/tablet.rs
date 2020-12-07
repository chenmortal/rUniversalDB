use crate::common::rand::RandGen;
use crate::model::common::{Row, Schema, TabletShape};
use crate::model::message::{
  AdminMessage, AdminRequest, AdminResponse, NetworkMessage, SelectPrepare, SlaveMessage,
  TabletAction, TabletMessage,
};
use crate::storage::relational_tablet::RelationalTablet;

#[derive(Debug)]
pub struct TabletSideEffects {
  pub actions: Vec<TabletAction>,
}

impl TabletSideEffects {
  pub fn new() -> TabletSideEffects {
    TabletSideEffects {
      actions: Vec::new(),
    }
  }

  pub fn add(&mut self, action: TabletAction) {
    self.actions.push(action);
  }
}

#[derive(Debug)]
pub struct TabletState {
  pub rand_gen: RandGen,
  pub this_shape: TabletShape,
  pub relational_tablet: RelationalTablet,
}

impl TabletState {
  pub fn new(rand_gen: RandGen, this_shape: TabletShape, schema: Schema) -> TabletState {
    TabletState {
      rand_gen,
      this_shape,
      relational_tablet: RelationalTablet::new(schema),
    }
  }

  pub fn handle_incoming_message(
    &mut self,
    side_effects: &mut TabletSideEffects,
    msg: TabletMessage,
  ) {
    match &msg {
      TabletMessage::AdminRequest { eid, req } => {
        match req {
          AdminRequest::Insert {
            rid,
            key,
            value,
            timestamp,
            ..
          } => {
            let row = Row {
              key: key.clone(),
              val: value.clone(),
            };
            let result = self.relational_tablet.insert_row(&row, *timestamp);
            side_effects.add(TabletAction::Send {
              eid: eid.clone(),
              msg: NetworkMessage::Admin(AdminMessage::AdminResponse {
                res: AdminResponse::Insert {
                  rid: rid.clone(),
                  result,
                },
              }),
            });
          }
          AdminRequest::Read {
            rid,
            key,
            timestamp,
            ..
          } => {
            let result = self.relational_tablet.read_row(&key, *timestamp);
            side_effects.add(TabletAction::Send {
              eid: eid.clone(),
              msg: NetworkMessage::Admin(AdminMessage::AdminResponse {
                res: AdminResponse::Read {
                  rid: rid.clone(),
                  result,
                },
              }),
            });
          }
          _ => panic!("The message {:?} shouldn't be forwarded here.", msg),
        };
      }
      TabletMessage::ClientRequest { .. } => {
        panic!("Can't handle client messages yet.");
      }
      TabletMessage::SelectPrepare(_) => {
        panic!("Preparing is not supported yet.");
      }
    }
  }
}
