use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use sha1::{Digest, Sha1};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

use crate::peer::{Message, PeerConnection, STANDARD_BLOCK_SIZE};
use crate::torrent::Torrent;
use crate::tracker;

pub async fn download_piece_from_peer(
    peer: &mut PeerConnection,
    torrent: &Torrent,
    piece_index: usize,
) -> Result<Vec<u8>> {
    peer.wait_for_unchoke(Duration::from_secs(15)).await?;

    let piece_size = torrent.piece_size(piece_index);
    let mut piece_buf = vec![0u8; piece_size];
    let mut downloaded = 0usize;
    let mut requested = 0usize;

    // Pipeline up to 5 requests
    const MAX_PIPELINE: usize = 5;
    let mut in_flight = 0usize;

    while downloaded < piece_size {
        // Send block requests while in_flight < MAX_PIPELINE and requested < piece_size
        while in_flight < MAX_PIPELINE && requested < piece_size {
            let block_len = std::cmp::min(STANDARD_BLOCK_SIZE, piece_size - requested);
            peer.send_message(&Message::Request {
                index: piece_index as u32,
                begin: requested as u32,
                length: block_len as u32,
            })
            .await?;

            requested += block_len;
            in_flight += 1;
        }

        // Wait for incoming piece block
        let msg = timeout(Duration::from_secs(10), peer.read_message())
            .await
            .context("Timed out waiting for block")??;

        match msg {
            Message::Piece { index, begin, block } => {
                if index as usize != piece_index {
                    continue;
                }
                let begin = begin as usize;
                if begin + block.len() <= piece_size {
                    piece_buf[begin..begin + block.len()].copy_from_slice(&block);
                    downloaded += block.len();
                    in_flight = in_flight.saturating_sub(1);
                }
            }
            Message::Choke => {
                peer.choked = true;
                peer.wait_for_unchoke(Duration::from_secs(15)).await?;
            }
            _ => {}
        }
    }

    // Verify SHA-1 hash
    let mut hasher = Sha1::new();
    hasher.update(&piece_buf);
    let computed_hash: [u8; 20] = hasher.finalize().into();

    if computed_hash != torrent.pieces[piece_index] {
        bail!(
            "Piece {} SHA-1 verification failed: expected {}, got {}",
            piece_index,
            hex::encode(torrent.pieces[piece_index]),
            hex::encode(computed_hash)
        );
    }

    Ok(piece_buf)
}

pub async fn download_single_piece<P: AsRef<Path>>(
    torrent: &Torrent,
    piece_index: usize,
    output_path: P,
) -> Result<()> {
    let client_peer_id = tracker::generate_peer_id();
    println!("Connecting to trackers to find peers...");
    let peers = tracker::get_peers(torrent, &client_peer_id, 6881).await?;
    println!("Discovered {} peer(s)", peers.len());

    let mut last_err = None;
    for peer_addr in peers {
        println!("Attempting connection to {}...", peer_addr);
        match PeerConnection::connect(peer_addr, torrent.info_hash, client_peer_id, Duration::from_secs(5)).await {
            Ok(mut conn) => {
                println!("Connected to peer {}. Downloading piece {}...", peer_addr, piece_index);
                match download_piece_from_peer(&mut conn, torrent, piece_index).await {
                    Ok(data) => {
                        std::fs::write(output_path, &data)?;
                        println!("Successfully downloaded and verified piece {}!", piece_index);
                        return Ok(());
                    }
                    Err(e) => {
                        eprintln!("Failed to download from {}: {:#}", peer_addr, e);
                        last_err = Some(e);
                    }
                }
            }
            Err(e) => {
                last_err = Some(e);
            }
        }
    }

    bail!("Failed to download piece from any peer: {:?}", last_err)
}

pub async fn download_all<P: AsRef<Path>>(
    torrent: &Torrent,
    output_path: P,
) -> Result<()> {
    let client_peer_id = tracker::generate_peer_id();
    println!("Querying trackers for peers...");
    let peers = tracker::get_peers(torrent, &client_peer_id, 6881).await?;
    println!("Discovered {} peer(s) in swarm", peers.len());

    if peers.is_empty() {
        bail!("No peers available to download from");
    }

    // Initialize destination file
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(output_path.as_ref())?;
    file.set_len(torrent.length as u64)?;
    let shared_file = Arc::new(std::sync::Mutex::new(file));

    // Progress bar
    let pb = ProgressBar::new(torrent.length as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta}) {msg}")
            .unwrap()
            .progress_chars("#>-"),
    );
    pb.set_message("Downloading");

    // Queue of piece indices
    let num_pieces = torrent.num_pieces();
    let queue: VecDeque<usize> = (0..num_pieces).collect();
    let shared_queue = Arc::new(Mutex::new(queue));

    // Spawn concurrent peer workers
    let max_workers = std::cmp::min(16, peers.len());
    let mut handles = Vec::new();

    for peer_addr in peers.into_iter().take(max_workers) {
        let torrent = torrent.clone();
        let queue = Arc::clone(&shared_queue);
        let shared_file = Arc::clone(&shared_file);
        let pb = pb.clone();

        let handle = tokio::spawn(async move {
            run_worker(peer_addr, torrent, queue, shared_file, client_peer_id, pb).await
        });
        handles.push(handle);
    }

    for handle in handles {
        let _ = handle.await;
    }

    let remaining = shared_queue.lock().await.len();
    if remaining > 0 {
        bail!("Download incomplete: {} piece(s) could not be downloaded", remaining);
    }

    pb.finish_with_message("Download complete!");
    println!("\nFile successfully saved to: {}", output_path.as_ref().display());
    Ok(())
}

async fn run_worker(
    addr: SocketAddr,
    torrent: Torrent,
    queue: Arc<Mutex<VecDeque<usize>>>,
    shared_file: Arc<std::sync::Mutex<File>>,
    client_peer_id: [u8; 20],
    pb: ProgressBar,
) {
    let mut conn = match PeerConnection::connect(addr, torrent.info_hash, client_peer_id, Duration::from_secs(6)).await {
        Ok(c) => c,
        Err(_) => return,
    };

    loop {
        // Pop next piece index
        let piece_idx = {
            let mut q = queue.lock().await;
            q.pop_front()
        };

        let piece_idx = match piece_idx {
            Some(idx) => idx,
            None => break, // Queue is empty, all pieces are done or in progress
        };

        // Download piece
        match download_piece_from_peer(&mut conn, &torrent, piece_idx).await {
            Ok(data) => {
                // Write piece to file
                let offset = piece_idx as u64 * torrent.piece_length as u64;
                let piece_len = data.len();

                {
                    use std::io::{Seek, SeekFrom, Write};
                    let mut file = shared_file.lock().unwrap();
                    if let Ok(_) = file.seek(SeekFrom::Start(offset)) {
                        let _ = file.write_all(&data);
                    }
                }

                pb.inc(piece_len as u64);
            }
            Err(_) => {
                // Return piece back to queue for another peer to pick up
                let mut q = queue.lock().await;
                q.push_back(piece_idx);
                break; // Peer errored or disconnected, terminate this worker
            }
        }
    }
}
