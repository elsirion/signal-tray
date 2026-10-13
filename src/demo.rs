//! Sample conversations for `--demo`, to try the UI without a Signal
//! account: a personal and a work account.

use std::path::{Path, PathBuf};

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::config::{AccountConfig, Color};
use crate::model::{
    AccountId, AttachmentKind, Author, ConversationId, NewAttachment, NewMessage, PeerId, Timestamp,
};
use crate::store::{ReadState, Store, StoredAvatar};

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

type Sample<'a> = (&'a ConversationId, Option<&'a str>, i64, &'a str, ReadState);

/// The demo accounts with their in-memory stores and image directories,
/// which are temporary.
pub fn accounts() -> anyhow::Result<Vec<(AccountConfig, Store, PathBuf)>> {
    let account = |id: &str, name: &str, color: Color| AccountConfig {
        id: AccountId::try_from(id.to_owned()).expect("valid account id"),
        name: name.to_owned(),
        color,
    };
    let dir = glib::tmp_dir().join(format!("signal-tray-demo-{}", std::process::id()));
    let (personal_dir, work_dir) = (dir.join("1"), dir.join("2"));
    std::fs::create_dir_all(&personal_dir)?;
    std::fs::create_dir_all(&work_dir)?;
    let mut personal = Store::open_in_memory()?;
    populate_personal(&mut personal, &personal_dir)?;
    let mut work = Store::open_in_memory()?;
    populate_work(&mut work)?;
    Ok(vec![
        (
            account("1", "Personal", Color::PALETTE[0]),
            personal,
            personal_dir,
        ),
        (account("2", "Work", Color::PALETTE[1]), work, work_dir),
    ])
}

fn peer(id: &str) -> PeerId {
    PeerId(id.to_owned())
}

fn direct(id: &str) -> ConversationId {
    ConversationId::Direct(peer(id))
}

fn populate_work(store: &mut Store) -> anyhow::Result<()> {
    let dave = direct("dave");
    let team = ConversationId::Group("team".into());
    store.upsert_names([
        (&dave, "Dave"),
        (&team, "Platform team"),
        (&direct("grace"), "Grace Hopper"),
    ])?;
    insert(
        store,
        &[
            (
                &dave,
                Some("dave"),
                3 * DAY,
                "Can you review the PR before Monday?",
                ReadState::Read,
            ),
            (
                &dave,
                None,
                3 * DAY - 5 * MINUTE,
                "Sure, will do tomorrow morning",
                ReadState::Read,
            ),
            (
                &team,
                Some("grace"),
                50 * MINUTE,
                "Deploy is green, thanks all",
                ReadState::Read,
            ),
            (
                &team,
                Some("dave"),
                5 * MINUTE,
                "Standup moved to 10:30 tomorrow",
                ReadState::Unread,
            ),
        ],
    )
}

fn populate_personal(store: &mut Store, images: &Path) -> anyhow::Result<()> {
    let alice = direct("alice");
    let bob = direct("bob");
    let family = ConversationId::Group("family".into());
    let carol = direct("carol");
    store.upsert_names([
        (&alice, "Alice Liddell"),
        (&bob, "Bob"),
        (&family, "Family 🏡"),
        (&carol, "Carol Danvers"),
        (&direct("erin"), "Erin"),
        (&direct("frank"), "Frank Zappa"),
        (&ConversationId::Group("climbing".into()), "Climbing crew"),
    ])?;

    let messages: &[Sample<'_>] = &[
        (
            &carol,
            Some("carol"),
            26 * HOUR - MINUTE,
            "look what I found in the attic",
            ReadState::Read,
        ),
        (&carol, None, 25 * HOUR, "haha amazing", ReadState::Read),
        (
            &family,
            Some("mum"),
            5 * HOUR,
            "Dinner on Sunday at 7?",
            ReadState::Read,
        ),
        (
            &family,
            Some("dad"),
            5 * HOUR - 10 * MINUTE,
            "👍",
            ReadState::Read,
        ),
        (
            &family,
            None,
            4 * HOUR,
            "I'll bring dessert",
            ReadState::Read,
        ),
        (
            &family,
            Some("mum"),
            40 * MINUTE,
            "Perfect, see you then ❤️",
            ReadState::Unread,
        ),
        (
            &bob,
            Some("bob"),
            2 * HOUR,
            "are we still on for tonight?",
            ReadState::Read,
        ),
        (
            &bob,
            None,
            2 * HOUR - 3 * MINUTE,
            "yes! 8pm at the usual place",
            ReadState::Read,
        ),
        (
            &alice,
            Some("alice"),
            12 * MINUTE,
            "Did you see the new release? Notes are at https://example.com/releases/0.2 (and www.example.org/changelog).",
            ReadState::Read,
        ),
        (
            &alice,
            None,
            10 * MINUTE,
            "Not yet, anything interesting? https://example.com/faq",
            ReadState::Read,
        ),
        (
            &alice,
            Some("alice"),
            2 * MINUTE,
            "Tons. The keyboard navigation is finally usable, and the whole thing fits in a corner of the screen now.",
            ReadState::Unread,
        ),
        (
            &alice,
            Some("alice"),
            MINUTE,
            "Try Alt+↓ to jump to the next chat",
            ReadState::Unread,
        ),
    ];
    insert(store, messages)?;
    insert_photo(store, &carol, "carol", 26 * HOUR, images)?;
    // Two in a row, so the second plays after the first.
    insert_voice(
        store,
        &bob,
        "bob",
        31 * MINUTE,
        &[440.0, 554.0, 659.0],
        images,
    )?;
    insert_voice(store, &bob, "bob", 30 * MINUTE, &[659.0, 554.0], images)?;
    // Everyone else gets initials.
    let avatars = images.join("avatars");
    std::fs::create_dir_all(&avatars)?;
    insert_avatar(store, &alice, [230, 140, 60], &avatars)?;
    insert_avatar(store, &direct("mum"), [90, 160, 120], &avatars)
}

/// A generated picture: a light spot on a gradient of `rgb`.
fn insert_avatar(
    store: &mut Store,
    subject: &ConversationId,
    rgb: [u8; 3],
    dir: &Path,
) -> anyhow::Result<()> {
    const SIZE: usize = 96;
    let mut pixels = Vec::with_capacity(SIZE * SIZE * 3);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (dx, dy) = (x as f64 - 48.0, y as f64 - 40.0);
            let spot = (1.0 - (dx * dx + dy * dy).sqrt() / 30.0).max(0.0);
            let shade = 0.6 + 0.4 * (y as f64 / SIZE as f64);
            for c in rgb {
                let v = f64::from(c) * shade + (255.0 - f64::from(c) * shade) * spot;
                pixels.push(v.round().clamp(0.0, 255.0) as u8);
            }
        }
    }
    let texture = gdk::MemoryTexture::new(
        SIZE as i32,
        SIZE as i32,
        gdk::MemoryFormat::R8g8b8,
        &glib::Bytes::from_owned(pixels),
        SIZE * 3,
    );
    let file = format!("{}.png", subject.to_string().replace([':', '/'], "_"));
    texture.save_to_png(dir.join(&file))?;
    store.set_avatar(
        subject,
        &StoredAvatar {
            file: Some(file),
            fetched_at: Timestamp(glib::real_time() / 1000),
        },
    )?;
    Ok(())
}

/// A generated voice message from `from`, downloaded but not played: a
/// beep per tone, 0.6 s each.
fn insert_voice(
    store: &mut Store,
    conversation: &ConversationId,
    from: &str,
    age: i64,
    tones: &[f64],
    dir: &Path,
) -> anyhow::Result<()> {
    let timestamp = Timestamp(glib::real_time() / 1000 - age);
    let author = Author::Peer(peer(from));
    store.insert_message(
        &NewMessage {
            conversation: conversation.clone(),
            author: author.clone(),
            author_name: Some(capitalize(from)),
            timestamp,
            body: String::new(),
            attachments: vec![NewAttachment {
                kind: AttachmentKind::Voice,
                content_type: "audio/wav".into(),
                file_name: None,
                width: None,
                height: None,
                pointer: Vec::new(),
            }],
            expires_in: None,
        },
        ReadState::Unread,
    )?;
    let wav = beeps(tones);
    for id in store.message_attachments(conversation, &author, timestamp)? {
        let file = format!("{}.wav", id.0);
        std::fs::write(dir.join(&file), &wav)?;
        store.set_attachment_file(id, &file)?;
    }
    Ok(())
}

/// A mono 16 kHz, 16-bit WAV file.
fn beeps(tones: &[f64]) -> Vec<u8> {
    const RATE: u32 = 16_000;
    const TONE: usize = RATE as usize * 6 / 10;
    let mut samples = Vec::with_capacity(tones.len() * TONE);
    for tone in tones {
        for i in 0..TONE {
            let t = i as f64 / f64::from(RATE);
            // Fades in and out, so tones don't click.
            let envelope = (std::f64::consts::PI * i as f64 / TONE as f64).sin();
            let sample = (2.0 * std::f64::consts::PI * tone * t).sin() * envelope * 0.3;
            samples.push((sample * f64::from(i16::MAX)) as i16);
        }
    }
    let data_len = u32::try_from(samples.len() * 2).expect("seconds of audio fit");
    let mut wav = Vec::with_capacity(44 + samples.len() * 2);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes()); // bytes per second
    wav.extend_from_slice(&2u16.to_le_bytes()); // bytes per frame
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    wav
}

/// A generated "photo" from `from`, already downloaded.
fn insert_photo(
    store: &mut Store,
    conversation: &ConversationId,
    from: &str,
    age: i64,
    dir: &Path,
) -> anyhow::Result<()> {
    const WIDTH: usize = 320;
    const HEIGHT: usize = 240;
    let timestamp = Timestamp(glib::real_time() / 1000 - age);
    let author = Author::Peer(peer(from));
    store.insert_message(
        &NewMessage {
            conversation: conversation.clone(),
            author: author.clone(),
            author_name: Some(capitalize(from)),
            timestamp,
            body: String::new(),
            attachments: vec![NewAttachment {
                kind: AttachmentKind::Image,
                content_type: "image/png".into(),
                file_name: Some("IMG_2041.png".into()),
                width: Some(WIDTH as u32),
                height: Some(HEIGHT as u32),
                pointer: Vec::new(),
            }],
            expires_in: None,
        },
        ReadState::Read,
    )?;
    // A dusky sky over a dark hill.
    let mut pixels = Vec::with_capacity(WIDTH * HEIGHT * 3);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let hill = HEIGHT * 2 / 3 + (x * (WIDTH - x)) / (WIDTH * 4);
            let rgb = if y > HEIGHT - hill / 2 {
                [30, 50, 40]
            } else {
                let t = y * 255 / HEIGHT;
                [(90 + t / 2) as u8, (60 + t / 3) as u8, (160 - t / 3) as u8]
            };
            pixels.extend_from_slice(&rgb);
        }
    }
    let texture = gdk::MemoryTexture::new(
        WIDTH as i32,
        HEIGHT as i32,
        gdk::MemoryFormat::R8g8b8,
        &glib::Bytes::from_owned(pixels),
        WIDTH * 3,
    );
    for id in store.message_attachments(conversation, &author, timestamp)? {
        let file = format!("{}.png", id.0);
        texture.save_to_png(dir.join(&file))?;
        store.set_attachment_file(id, &file)?;
    }
    Ok(())
}

fn insert(store: &mut Store, messages: &[Sample<'_>]) -> anyhow::Result<()> {
    let now = glib::real_time() / 1000;
    for (conversation, author, age, body, read) in messages {
        let author = match author {
            Some(id) => Author::Peer(peer(id)),
            None => Author::Me,
        };
        store.insert_message(
            &NewMessage {
                conversation: (*conversation).clone(),
                author_name: match &author {
                    Author::Peer(p) => Some(capitalize(&p.0)),
                    Author::Me => None,
                },
                author,
                timestamp: Timestamp(now - age),
                body: (*body).to_owned(),
                attachments: Vec::new(),
                expires_in: None,
            },
            *read,
        )?;
    }
    Ok(())
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
