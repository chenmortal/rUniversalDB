use clap::{arg, App};
use rand::{RngCore, SeedableRng};
use rand_xorshift::XorShiftRng;
use runiversal::common::mk_rid;
use runiversal::model::common::{EndpointId, RequestId};
use runiversal::model::message as msg;
use runiversal::net::{recv, send_msg, SERVER_PORT};
use std::collections::BTreeMap;
use std::io::SeekFrom::End;
use std::io::Write;
use std::net::TcpListener;
use std::sync::mpsc::Sender;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::SystemTime;

// The Threading Model is the same as the Transact Server.

fn prompt(name: &str) -> String {
  let mut line = String::new();
  print!("{}", name);
  std::io::stdout().flush().unwrap();
  std::io::stdin().read_line(&mut line).expect("Error: Could not read a line");
  return line.trim().to_string();
}

fn main() {
  // Setup CLI parsing
  let matches = App::new("rUniversalDB")
    .version("1.0")
    .author("Pasindu M. <pasindumuth@gmail.com>")
    .arg(arg!(-i --ip <VALUE>).required(true).help("The IP address of the current host."))
    .get_matches();

  // Get required arguments
  let this_ip = matches.value_of("ip").unwrap().to_string();

  // The mpsc channel for passing data to the Server Thread from all FromNetwork Threads.
  let (to_server_sender, to_server_receiver) = mpsc::channel::<(EndpointId, msg::NetworkMessage)>();
  // Maps the IP addresses to a FromServer Queue, used to send data to Outgoing Connections.
  let out_conn_map = Arc::new(Mutex::new(BTreeMap::<EndpointId, Sender<Vec<u8>>>::new()));
  // Create an RNG for ID generation
  let mut rand = XorShiftRng::from_entropy();

  // Start the Accepting Thread
  {
    let to_server_sender = to_server_sender.clone();
    let this_ip = this_ip.clone();
    thread::spawn(move || {
      let listener = TcpListener::bind(format!("{}:{}", &this_ip, SERVER_PORT)).unwrap();
      for stream in listener.incoming() {
        let stream = stream.unwrap();
        let other_ip = EndpointId(stream.peer_addr().unwrap().ip().to_string());

        // Setup FromNetwork Thread
        {
          let to_server_sender = to_server_sender.clone();
          let stream = stream.try_clone().unwrap();
          let other_ip = other_ip.clone();
          thread::spawn(move || loop {
            let data = recv(&stream);
            let network_msg: msg::NetworkMessage = rmp_serde::from_read_ref(&data).unwrap();
            to_server_sender.send((other_ip.clone(), network_msg)).unwrap();
          });
        }

        println!("Connected from: {:?}", other_ip);
      }
    });
  }

  // The EndpointId of this node
  let this_eid = EndpointId(this_ip);
  // The Master EndpointIds we tried starting the Master with
  let mut master_eids = Vec::<EndpointId>::new();
  // The EndpointId that most communication should use.
  let mut opt_target_eid = Option::<EndpointId>::None;

  // Setup the CLI read loop.
  loop {
    let input = prompt("> ");
    match input.split_once(" ") {
      Some(("startmaster", rest)) => {
        // Start the masters
        master_eids = rest.split(" ").into_iter().map(|ip| EndpointId(ip.to_string())).collect();
        for eid in &master_eids {
          send_msg(
            &out_conn_map,
            eid,
            msg::NetworkMessage::FreeNode(msg::FreeNodeMessage::StartMaster(msg::StartMaster {
              master_eids: master_eids.clone(),
            })),
          );
        }
      }
      _ => {
        if input == "exit" {
          break;
        } else {
          if let Some(target_eid) = &opt_target_eid {
            match input.split_once(" ") {
              Some(("target", rest)) => {
                opt_target_eid = Some(EndpointId(rest.to_string()));
              }
              _ => {
                // Send the message.
                let request_id = mk_rid(&mut rand);
                send_msg(
                  &out_conn_map,
                  &target_eid,
                  msg::NetworkMessage::Master(msg::MasterMessage::MasterExternalReq(
                    msg::MasterExternalReq::PerformExternalDDLQuery(msg::PerformExternalDDLQuery {
                      sender_eid: this_eid.clone(),
                      request_id,
                      query: input,
                    }),
                  )),
                );

                // Wait for a response. We assume the first response is for the
                // request we just sent above.
                let (_, message) = to_server_receiver.recv().unwrap();
                print!("{:#?}", message);
              }
            }
          } else {
            print!("A target address is not set. Do that by typing 'target <hostname>'.\n");
          }
        }
      }
    }
  }
}
