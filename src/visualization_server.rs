use futures::{Stream, StreamExt};
use r2r::builtin_interfaces::msg::Duration;
use r2r::scene_manipulation_msgs::msg::TFExtra;

use r2r::visualization_msgs::msg::{Marker, MarkerArray};
use r2r::QosProfile;
use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex};

use r2r::geometry_msgs::msg::{Point, Pose, Quaternion, Transform};
use r2r::std_msgs::msg::{ColorRGBA, Header};
use r2r::{builtin_interfaces::msg::Time, geometry_msgs::msg::Vector3};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub static NODE_ID: &'static str = "visualization_server";
pub static BUFFER_MAINTAIN_RATE: u64 = 100;
pub static MARKER_PUBLISH_RATE: u64 = 100;
pub static FRAME_LIFETIME: i32 = 3; //seconds

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct LoadedFrameData {
    // mandatory fields in the json files
    pub parent_frame_id: String, // the id of the frame's parent frame
    pub child_frame_id: String,  // the id of the frame
    pub transform: Transform,    // where is the child frame defined in the parent
    // optional fields in the json files. will be encoded to a json string before sent out
    pub extra_data: Option<ExtraData>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct FrameData {
    // mandatory fields in the json files
    pub parent_frame_id: String, // the id of the frame's parent frame
    pub child_frame_id: String,  // the id of the frame
    pub transform: Transform,    // where is the child frame defined in the parent
    // optional fields in the json files. will be encoded to a json string before sent out
    pub extra_data: ExtraData,
}

// #[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
// pub struct MeshColor {
//     pub r: f32,
//     pub g: f32,
//     pub b: f32,
//     pub a: f32,
// }

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ExtraData {
    pub time_stamp: Option<Time>, // the idea is that all frames should have this, but some don't
    pub zone: Option<f64>,        // when are you "at" the frame, threshold, in meters
    pub next: Option<HashSet<String>>, // this list can be used to store data for planners and visualizers
    pub frame_type: Option<String>, // can be used to distinguish if a frame is a waypoint, tag, human, etc.
    pub active: Option<bool>, // only active frames are manipulatable. undefined will be added as active
    pub show_mesh: Option<bool>, // if the frame should also visualize something or not
    pub mesh_type: Option<i32>, // 1 - cube, 2 - sphere, 3 - cylinder or 10 - mesh (provide path)
    pub mesh_path: Option<String>, // where to find the mesh path if mesh_type was 10
    pub mesh_scale: Option<f32>, // not all meshes are in mm values
    // pub mesh_color: Option<MeshColor>, // color for the mesh, A is transparency
    pub mesh_r: Option<f32>,
    pub mesh_g: Option<f32>,
    pub mesh_b: Option<f32>,
    pub mesh_a: Option<f32>,
}

impl Default for ExtraData {
    fn default() -> Self {
        ExtraData {
            time_stamp: None,
            zone: None,
            next: None,
            frame_type: None,
            active: None,
            show_mesh: None,
            mesh_type: None,
            mesh_path: None,
            mesh_scale: None,
            // mesh_color: None,
            mesh_r: None,
            mesh_g: None,
            mesh_b: None,
            mesh_a: None,
        }
    }
}

impl Default for FrameData {
    fn default() -> Self {
        FrameData {
            parent_frame_id: "world".to_string(),
            child_frame_id: "UNKNOWN".to_string(),
            transform: Transform {
                translation: Vector3 {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                rotation: Quaternion {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                    w: 1.0,
                },
            },
            // json_path: "".to_string(),
            extra_data: ExtraData::default(),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // setup the node
    let ctx = r2r::Context::create()?;
    let mut node = r2r::Node::create(ctx, NODE_ID, "")?;

    // a buffer of frames that exist on the tf_extra topic
    let buffered_frames = Arc::new(Mutex::new(HashMap::<String, FrameData>::new()));

    // listen to the frames on the tf_extra topic
    // spawn a tokio task to listen to the frames and add them to the buffer
    let extra_tf_listener =
        node.subscribe::<TFExtra>("tf_extra", QosProfile::best_effort(QosProfile::default()))?;
    let buffered_frames_clone = buffered_frames.clone();
    tokio::task::spawn(async move {
        match extra_tf_listener_callback(extra_tf_listener, &buffered_frames_clone.clone(), NODE_ID)
            .await
        {
            Ok(()) => (),
            Err(e) => r2r::log_error!(NODE_ID, "Extra tf listener failed with: '{}'.", e),
        };
    });

    // spawn a tokio task to maintain the tf buffer by removing stale frames and adding fresh ones
    let buffer_maintain_timer =
        node.create_wall_timer(std::time::Duration::from_millis(BUFFER_MAINTAIN_RATE))?;
    let buffered_frames_clone = buffered_frames.clone();
    tokio::task::spawn(async move {
        match maintain_buffer(buffer_maintain_timer, &buffered_frames_clone).await {
            Ok(()) => (),
            Err(e) => r2r::log_error!(NODE_ID, "Buffer maintainer failed with: '{}'.", e),
        };
    });

    let marker_publisher_timer =
        node.create_wall_timer(std::time::Duration::from_millis(MARKER_PUBLISH_RATE))?;

    let zone_marker_publisher =
        node.create_publisher::<MarkerArray>("zone_markers", QosProfile::default())?;

    let mesh_marker_publisher =
        node.create_publisher::<MarkerArray>("mesh_markers", QosProfile::default())?;

    let buffered_frames_clone = buffered_frames.clone();
    tokio::task::spawn(async move {
        let result = visualization_server(
            mesh_marker_publisher,
            zone_marker_publisher,
            &buffered_frames_clone,
            marker_publisher_timer,
        )
        .await;
        match result {
            Ok(()) => r2r::log_info!(NODE_ID, "Visualization Server succeeded."),
            Err(e) => r2r::log_error!(NODE_ID, "Visualization Server failed with: {}.", e),
        };
    });

    // keep the node alive
    let handle = std::thread::spawn(move || loop {
        node.spin_once(std::time::Duration::from_millis(1000));
    });

    r2r::log_warn!(NODE_ID, "Node started.");

    handle.join().unwrap();

    Ok(())
}

pub async fn extra_tf_listener_callback(
    mut subscriber: impl Stream<Item = TFExtra> + Unpin,
    buffered_frames: &Arc<Mutex<HashMap<String, FrameData>>>,
    node_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        match subscriber.next().await {
            Some(message) => {
                let mut frames_local = buffered_frames.lock().unwrap().clone();
                message.data.iter().for_each(|t| {
                    let mut clock = r2r::Clock::create(r2r::ClockType::RosTime).unwrap();
                    let now = clock.get_now().unwrap();
                    let time_stamp = r2r::Clock::to_builtin_time(&now);

                    match serde_json::from_str(&t.extra) {
                        Ok::<ExtraData, _>(mut extras) => {
                            extras.time_stamp = Some(time_stamp);
                            frames_local.insert(
                                t.transform.child_frame_id.clone(),
                                FrameData {
                                    parent_frame_id: t.transform.header.frame_id.clone(),
                                    child_frame_id: t.transform.child_frame_id.clone(),
                                    transform: t.transform.transform.clone(),
                                    extra_data: extras,
                                },
                            );
                        }
                        Err(_) => todo!(),
                    }
                });
                *buffered_frames.lock().unwrap() = frames_local;
            }
            None => {
                r2r::log_error!(node_id, "Subscriber did not get the message?");
            }
        }
    }
}

pub async fn maintain_buffer(
    mut timer: r2r::Timer,
    buffered_frames: &Arc<Mutex<HashMap<String, FrameData>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        let frames_local = buffered_frames.lock().unwrap().clone();
        let mut frames_local_reduced = frames_local.clone();
        let mut clock = r2r::Clock::create(r2r::ClockType::RosTime).unwrap();
        let now = clock.get_now().unwrap();
        let current_time = r2r::Clock::to_builtin_time(&now);
        frames_local.iter().for_each(|(k, v)| {
            let stamp = v.clone().extra_data.time_stamp.unwrap(); // TODO: handle this nicer
            match v.extra_data.active {
                Some(true) | None => match current_time.sec > stamp.sec + FRAME_LIFETIME {
                    true => {
                        frames_local_reduced.remove(k);
                    }
                    false => (), // do nothing if the frame is fresh
                },
                Some(false) => (),
            }
        });
        *buffered_frames.lock().unwrap() = frames_local_reduced;
        timer.tick().await?;
    }
}

pub async fn visualization_server(
    mesh_publisher: r2r::Publisher<MarkerArray>,
    zone_publisher: r2r::Publisher<MarkerArray>,
    // path_publisher: r2r::Publisher<MarkerArray>,
    buffered_frames: &Arc<Mutex<HashMap<String, FrameData>>>,
    mut timer: r2r::Timer,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        let mut mesh_markers: Vec<Marker> = vec![];
        let mut zone_markers: Vec<Marker> = vec![];
        let frames_local = buffered_frames.lock().unwrap().clone();
        let mut id = 0;
        for frame in frames_local {
            match frame.1.extra_data.zone {
                Some(z) => {
                    match z == 0.0 {
                        false => {
                            id = id + 1;
                            let indiv_marker = Marker {
                                header: Header {
                                    stamp: Time { sec: 0, nanosec: 0 },
                                    frame_id: frame.1.child_frame_id.to_string(),
                                },
                                ns: "".to_string(),
                                id,
                                type_: 2,
                                action: 0,
                                pose: Pose {
                                    position: Point {
                                        x: 0.0,
                                        y: 0.0,
                                        z: 0.0,
                                    },
                                    orientation: Quaternion {
                                        x: 0.0,
                                        y: 0.0,
                                        z: 0.0,
                                        w: 1.0,
                                    },
                                },
                                lifetime: Duration { sec: 2, nanosec: 0 },
                                scale: Vector3 { x: z, y: z, z },
                                color: ColorRGBA {
                                    r: 0.0,
                                    g: 255.0,
                                    b: 0.0,
                                    a: 0.15,
                                },
                                ..Marker::default()
                            };
                            zone_markers.push(indiv_marker)
                        }
                        true => (),
                    };
                }
                None => (),
            }
            match frame.1.extra_data.show_mesh {
                Some(false) | None => (),
                Some(true) => {
                    id = id + 1;
                    let indiv_marker = Marker {
                        header: Header {
                            stamp: Time { sec: 0, nanosec: 0 },
                            frame_id: frame.1.child_frame_id.to_string(),
                        },
                        ns: "".to_string(),
                        id,
                        type_: match frame.1.extra_data.mesh_type {
                            Some(1) => 1,
                            Some(2) => 2,
                            Some(3) => 3,
                            Some(10) => 10,
                            None => 1,
                            Some(_) => 1,
                        },
                        action: 0,
                        pose: Pose {
                            position: Point {
                                x: 0.0,
                                y: 0.0,
                                z: 0.0,
                            },
                            orientation: Quaternion {
                                x: 0.0,
                                y: 0.0,
                                z: 0.0,
                                w: 1.0,
                            },
                        },
                        lifetime: Duration { sec: 2, nanosec: 0 },
                        scale: match frame.1.extra_data.mesh_scale {
                            Some(scale) => Vector3 {
                                x: if scale != 0.0 {scale as f64} else {1.0},
                                y: if scale != 0.0 {scale as f64} else {1.0},
                                z: if scale != 0.0 {scale as f64} else {1.0},
                            },
                            None => Vector3 {
                                x: 1.0,
                                y: 1.0,
                                z: 1.0,
                            },
                        },
                        color: ColorRGBA {
                            r: match frame.1.extra_data.mesh_r {
                                Some(x) => x,
                                None => 0.0,
                            },
                            g: match frame.1.extra_data.mesh_g {
                                Some(x) => x,
                                None => 0.0,
                            },
                            b: match frame.1.extra_data.mesh_b {
                                Some(x) => x,
                                None => 0.0,
                            },
                            a: match frame.1.extra_data.mesh_a {
                                Some(x) => x,
                                None => 1.0,
                            },
                        },
                        mesh_resource: match frame.1.extra_data.mesh_path {
                            Some(path) => format!("file://{}", path.to_string()),
                            None => "".to_string(),
                        },
                        ..Marker::default()
                    };
                    mesh_markers.push(indiv_marker);
                }
            }
        }

        let zone_array_msg = MarkerArray {
            markers: zone_markers,
        };
        let mesh_array_msg = MarkerArray {
            markers: mesh_markers,
        };

        match zone_publisher.publish(&zone_array_msg) {
            Ok(()) => (),
            Err(e) => {
                r2r::log_error!(
                    NODE_ID,
                    "Publisher failed to send zone marker message with: {}",
                    e
                );
            }
        };

        match mesh_publisher.publish(&mesh_array_msg) {
            Ok(()) => (),
            Err(e) => {
                r2r::log_error!(
                    NODE_ID,
                    "Publisher failed to send mesh marker message with: {}",
                    e
                );
            }
        };

        timer.tick().await?;
    }
}
