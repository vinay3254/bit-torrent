use std::path::PathBuf;
use anyhow::Result;
use clap::{Parser, Subcommand};

use bittorrent_rust::download;
use bittorrent_rust::torrent::Torrent;
use bittorrent_rust::tracker;

#[derive(Parser)]
#[command(name = "bittorrent-rust")]
#[command(about = "A high-performance BitTorrent client written from scratch in Rust", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Inspect metadata of a .torrent file
    Info {
        /// Path to the .torrent file
        torrent_file: PathBuf,
    },
    /// Query the tracker and list available peers
    Peers {
        /// Path to the .torrent file
        torrent_file: PathBuf,
    },
    /// Download a single piece from a peer and save to a file
    DownloadPiece {
        /// Path to the .torrent file
        torrent_file: PathBuf,
        /// Piece index to download
        #[arg(short, long)]
        piece: usize,
        /// Output file destination
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Download the complete file from the peer swarm
    Download {
        /// Path to the .torrent file
        torrent_file: PathBuf,
        /// Output file destination (defaults to torrent's internal name)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Info { torrent_file } => {
            let torrent = Torrent::from_file(&torrent_file)?;
            println!("==================================================");
            println!(" Torrent Metadata: {}", torrent_file.display());
            println!("==================================================");
            println!("File Name:        {}", torrent.name);
            println!("Tracker URL:      {}", torrent.announce);
            if torrent.announce_list.len() > 1 {
                println!("Backup Trackers:  {} trackers", torrent.announce_list.len());
                for (i, trk) in torrent.announce_list.iter().enumerate().skip(1).take(5) {
                    println!("  [{}] {}", i, trk);
                }
            }
            println!("Total Size:       {} bytes ({:.2} MB)", torrent.length, torrent.length as f64 / 1_048_576.0);
            println!("Info Hash:        {}", torrent.info_hash_hex);
            println!("Piece Length:     {} bytes", torrent.piece_length);
            println!("Number of Pieces: {}", torrent.num_pieces());
            println!("First Piece Hash: {}", hex::encode(torrent.pieces[0]));
            println!("==================================================");
        }

        Commands::Peers { torrent_file } => {
            let torrent = Torrent::from_file(&torrent_file)?;
            let peer_id = tracker::generate_peer_id();
            println!("Contacting trackers for '{}'...", torrent.name);
            let peers = tracker::get_peers(&torrent, &peer_id, 6881).await?;
            println!("\nFound {} active peer(s):", peers.len());
            for (idx, peer) in peers.iter().enumerate() {
                println!("  [{:02}] {}", idx + 1, peer);
            }
        }

        Commands::DownloadPiece {
            torrent_file,
            piece,
            output,
        } => {
            let torrent = Torrent::from_file(&torrent_file)?;
            if piece >= torrent.num_pieces() {
                anyhow::bail!(
                    "Piece index {} is out of range (total pieces: {})",
                    piece,
                    torrent.num_pieces()
                );
            }
            println!(
                "Downloading piece {} ({} bytes) to {}...",
                piece,
                torrent.piece_size(piece),
                output.display()
            );
            download::download_single_piece(&torrent, piece, output).await?;
        }

        Commands::Download {
            torrent_file,
            output,
        } => {
            let torrent = Torrent::from_file(&torrent_file)?;
            let out_path = output.unwrap_or_else(|| PathBuf::from(&torrent.name));
            println!("Starting download of '{}' ({:.2} MB)", torrent.name, torrent.length as f64 / 1_048_576.0);
            println!("Target output path: {}", out_path.display());
            download::download_all(&torrent, out_path).await?;
        }
    }

    Ok(())
}
