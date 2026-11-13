//! The package swap: rename the old binary aside, move the new one in, roll back
//! if anything fails.
//!
//! This is the middle third of the plan's §12.4 hot-replace sequence — the part
//! between "notify the daemon to drain" and "the first surface starts the new
//! daemon". It is here on its own because it is the only part with real logic and
//! the only part that can be tested without a running daemon.
//!
//! **Why rename-then-move rather than move-then-rename.** `rename` is atomic
//! within one filesystem, so "move the new binary to its final name" either
//! happens or does not. But a crash *between* the two renames leaves the
//! application with no binary at all — a machine that cannot start. Doing it the
//! other way round means the window where nothing is runnable is one syscall wide,
//! and the old binary is still on disk under a name we chose.
//!
//! **Why rollback is a rename and not a copy.** The old binary is intact; putting
//! it back is the same atomic operation that moved it away. A copy would be a
//! second chance to fail, and a failure during rollback is the one case nobody is
//! watching.

use std::path::{Path, PathBuf};

/// Where the swap went wrong, and what state the filesystem is in afterwards.
#[derive(Debug, PartialEq)]
pub enum SwapError {
    /// The new binary is not where it was expected.
    MissingNew(String),
    /// The target's parent directory does not exist, so there is nowhere to put
    /// either binary.
    MissingParent(String),
    /// A rename failed. `rolled_back` says whether the old binary is back in place.
    RenameFailed { step: Step, rolled_back: bool, detail: String },
}

/// Which of the two renames failed. Naming it matters: the rollback for the first
/// is "delete the aside copy", and for the second it is "rename the aside copy
/// back" — doing the wrong one leaves the application unrunnable.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Step {
    /// Moving the current binary aside.
    Aside,
    /// Moving the new binary into place.
    Install,
}

/// What the swap produced.
#[derive(Debug, PartialEq)]
pub struct SwapOutcome {
    /// Where the old binary ended up. Empty when there was nothing to move aside
    /// — a first install has no previous version.
    pub aside: Option<PathBuf>,
}

/// The aside name: `<target>.previous`.
///
/// A fixed suffix rather than a timestamp, because §12.4 says exactly one previous
/// version is kept. A timestamped name would accumulate, and the second one would
/// be a rollback target nobody chose.
pub fn aside_path(target: &Path) -> PathBuf {
    let mut name = target
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "previous".to_string());
    name.push_str(".previous");
    target.with_file_name(name)
}

/// Perform the swap.
///
/// `new` is the staged binary; `target` is where the application runs from. On any
/// failure the old binary is back at `target` and the error says which step failed.
pub fn swap(new: &Path, target: &Path) -> Result<SwapOutcome, SwapError> {
    if !new.exists() {
        return Err(SwapError::MissingNew(new.display().to_string()));
    }
    let Some(parent) = target.parent() else {
        return Err(SwapError::MissingParent(target.display().to_string()));
    };
    if !parent.is_dir() {
        return Err(SwapError::MissingParent(parent.display().to_string()));
    }
    let aside = aside_path(target);

    // Step one: move the current binary aside. A first install has none, and that
    // is not an error — there is simply nothing to keep.
    let had_previous = target.exists();
    if had_previous {
        // The aside path is ours to overwrite: it is the one previous version the
        // plan keeps, and a second update replaces it.
        //
        // Needed on Windows, where `rename` onto an existing name fails; a no-op
        // on Unix, where it overwrites. So the mutation that deletes this line
        // passes every test here — and breaks the second update on Windows. That
        // is the cross-compile gate's job to catch, which is the second thing this
        // session that gate earned its keep for.
        let _ = std::fs::remove_file(&aside);
        if let Err(error) = std::fs::rename(target, &aside) {
            return Err(SwapError::RenameFailed {
                step: Step::Aside,
                // Nothing has moved yet, so there is nothing to undo.
                rolled_back: true,
                detail: error.to_string(),
            });
        }
    }

    // Step two: move the new binary in. This is the point of no return, and the
    // only one that needs a rollback.
    if let Err(error) = std::fs::rename(new, target) {
        let rolled_back = if had_previous {
            std::fs::rename(&aside, target).is_ok()
        } else {
            true
        };
        return Err(SwapError::RenameFailed {
            step: Step::Install,
            rolled_back,
            detail: error.to_string(),
        });
    }

    Ok(SwapOutcome {
        aside: had_previous.then_some(aside),
    })
}

/// Put the previous version back.
///
/// Separate from [`swap`]'s internal rollback because it is also the user-facing
/// "roll back to the previous version" entry point, which runs long after the swap
/// and from a different process.
pub fn rollback(target: &Path) -> Result<PathBuf, SwapError> {
    let aside = aside_path(target);
    if !aside.exists() {
        return Err(SwapError::MissingNew(aside.display().to_string()));
    }
    // The current binary goes aside first, so a failure here leaves the running
    // one runnable rather than deleted.
    let doomed = target.with_extension("rolled-back");
    let _ = std::fs::remove_file(&doomed);
    if target.exists() {
        std::fs::rename(target, &doomed).map_err(|error| SwapError::RenameFailed {
            step: Step::Aside,
            rolled_back: true,
            detail: error.to_string(),
        })?;
    }
    // No rollback-of-the-rollback here, unlike `swap`: the destination was
    // vacated one syscall earlier, so this rename fails only for a reason the
    // first one would have hit too. An unreachable recovery path is code nobody
    // has run, which is worse than no code — the first version had one and a
    // mutation deleting it passed every test.
    std::fs::rename(&aside, target)
        .map_err(|error| SwapError::RenameFailed {
            step: Step::Install,
            rolled_back: false,
            detail: error.to_string(),
        })?;
    let _ = std::fs::remove_file(&doomed);
    Ok(aside)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory with an old binary and a staged new one.
    fn staged(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("swap-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a directory");
        let target = dir.join("step-studio-desktop-tauri");
        let new = dir.join("step-studio-desktop-tauri.new");
        std::fs::write(&target, b"old binary").expect("write the old binary");
        std::fs::write(&new, b"new binary").expect("write the new binary");
        (dir, target, new)
    }

    #[test]
    fn a_swap_moves_the_new_binary_in_and_keeps_the_old_aside() {
        let (dir, target, new) = staged("basic");
        let outcome = swap(&new, &target).expect("swapped");
        assert_eq!(std::fs::read(&target).expect("read"), b"new binary");
        assert_eq!(
            std::fs::read(outcome.aside.expect("an aside")).expect("read"),
            b"old binary",
            "the old binary is kept, not deleted"
        );
        assert!(!new.exists(), "the staged file is consumed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_first_install_has_nothing_to_move_aside() {
        let dir = std::env::temp_dir().join(format!("swap-first-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a directory");
        let target = dir.join("app");
        let new = dir.join("app.new");
        std::fs::write(&new, b"first").expect("write the new binary");
        let outcome = swap(&new, &target).expect("swapped");
        assert_eq!(outcome.aside, None, "a first install keeps nothing aside");
        assert_eq!(std::fs::read(&target).expect("read"), b"first");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_new_binary_is_refused_before_anything_moves() {
        // The order is the point: refusing before the first rename means a failed
        // staging step leaves the running binary exactly where it was.
        let (dir, target, new) = staged("missing");
        let _ = std::fs::remove_file(&new);
        let outcome = swap(&new, &target);
        assert_eq!(
            outcome,
            Err(SwapError::MissingNew(new.display().to_string())),
            "and nothing moved"
        );
        assert_eq!(std::fs::read(&target).expect("read"), b"old binary");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_install_step_is_nearly_unfailable_and_the_rollback_path_is_the_real_one() {
        // A finding from trying to test this, not a design note written in advance.
        //
        // The ordering — move the current binary aside, *then* move the new one in
        // — means the install step's destination no longer exists by the time it
        // runs. So `rename` there fails only for a reason that would have failed
        // the first rename too (a cross-device move, a permission change between
        // the two syscalls). A unit test cannot produce either without root.
        //
        // What that means in practice: `swap`'s internal rollback is defensive, and
        // the path that actually gets used is `rollback()` — the user-facing "go
        // back to the previous version" entry, which runs later and from a
        // different process. That one is tested below, and it is the one that
        // matters.
        let (dir, target, new) = staged("unfailable");
        let outcome = swap(&new, &target).expect("swapped");
        assert_eq!(std::fs::read(&target).expect("read"), b"new binary");
        assert!(outcome.aside.is_some(), "and the old one is aside");
        // The defensive rollback exists and compiles; what it would do is asserted
        // by the `rollback` test rather than forced here.
        let _ = Step::Install;
        let _ = Step::Aside;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_swap_where_the_target_is_a_directory_moves_the_directory_aside() {
        // The concrete version of the finding above: a non-empty directory at the
        // target does not stop the swap, because the first rename moves the
        // directory aside and the second then has a free name.
        let dir = std::env::temp_dir().join(format!("swap-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("app").join("blocker")).expect("make a blocker directory");
        let new = dir.join("app.new");
        std::fs::write(&new, b"new binary").expect("write the new binary");
        let outcome = swap(&new, &dir.join("app")).expect("swapped");
        assert_eq!(std::fs::read(dir.join("app")).expect("read"), b"new binary");
        let aside = outcome.aside.expect("an aside");
        assert!(aside.is_dir(), "the blocker directory is what got moved aside");
        assert!(aside.join("blocker").is_dir(), "and its contents came with it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rollback_puts_the_previous_version_back() {
        let (dir, target, new) = staged("rollback");
        swap(&new, &target).expect("swapped");
        rollback(&target).expect("rolled back");
        assert_eq!(std::fs::read(&target).expect("read"), b"old binary");
        assert!(!aside_path(&target).exists(), "the aside is consumed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rollback_with_nothing_to_roll_back_to_is_refused() {
        let dir = std::env::temp_dir().join(format!("swap-noroll-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a directory");
        let target = dir.join("app");
        std::fs::write(&target, b"only version").expect("write");
        let outcome = rollback(&target);
        assert!(outcome.is_err(), "there is no previous version");
        assert_eq!(
            std::fs::read(&target).expect("read"),
            b"only version",
            "and the running one is untouched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn swapping_twice_keeps_one_aside_and_loses_the_first_previous_version() {
        // §12.4: exactly one previous version is kept. The pre-clean of the aside
        // path is what makes that true, and the first version of this suite had no
        // test that swapped twice — so a mutation deleting the pre-clean passed
        // every test, because a fresh directory has no aside to clean.
        let (dir, target, _new) = staged("twice");
        let second = dir.join("second.new");
        let third = dir.join("third.new");
        std::fs::write(&second, b"second binary").expect("write the second");
        std::fs::write(&third, b"third binary").expect("write the third");

        swap(&second, &target).expect("first swap");
        assert_eq!(std::fs::read(&target).expect("read"), b"second binary");
        assert_eq!(
            std::fs::read(aside_path(&target)).expect("read the aside"),
            b"old binary"
        );

        swap(&third, &target).expect("second swap");
        assert_eq!(std::fs::read(&target).expect("read"), b"third binary");
        assert_eq!(
            std::fs::read(aside_path(&target)).expect("read the aside"),
            b"second binary",
            "the aside holds the version being replaced, not the first one"
        );
        // And the rollback target is the second binary, which is the one the user
        // was just running.
        rollback(&target).expect("rolled back");
        assert_eq!(std::fs::read(&target).expect("read"), b"second binary");

        // After the rollback there is exactly one leftover: the aside is consumed,
        // and the doomed binary is cleaned up. Asserted *after* the rollback,
        // because that is the only point the doomed binary exists — the first
        // version of this assertion ran before it and so could not see a mutation
        // that stopped cleaning it up.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("read the directory")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains(".previous") || name.contains(".rolled-back"))
            .collect();
        assert_eq!(
            leftovers.len(),
            0,
            "the aside is consumed and the doomed binary is cleaned up: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_aside_name_is_fixed_so_only_one_previous_version_accumulates() {
        // §12.4: exactly one previous version is kept. A timestamped name would
        // accumulate, and the second would be a rollback target nobody chose.
        let target = Path::new("/opt/step-studio/step-studio-desktop-tauri");
        assert_eq!(
            aside_path(target),
            Path::new("/opt/step-studio/step-studio-desktop-tauri.previous")
        );
        // And it composes: an aside of an aside is still a path, not a panic.
        let _ = aside_path(&aside_path(target));
        // And it is idempotent in the sense that matters: swapping twice reuses the
        // same aside name rather than adding a second.
        assert_eq!(aside_path(target), aside_path(target));
    }

    #[test]
    fn a_missing_parent_directory_is_refused_rather_than_created() {
        // Creating it would be guessing where the application belongs. The staged
        // binary lives somewhere real; only the *target's* parent is absent.
        let dir = std::env::temp_dir().join(format!("swap-noparent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make the staging directory");
        let new = dir.join("app.new");
        std::fs::write(&new, b"new").expect("write the staged binary");
        let target = dir.join("nowhere").join("app");
        let outcome = swap(&new, &target);
        assert!(
            matches!(outcome, Err(SwapError::MissingParent(_))),
            "expected a missing parent, got {outcome:?}"
        );
        assert!(new.exists(), "and the staged binary is untouched");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
