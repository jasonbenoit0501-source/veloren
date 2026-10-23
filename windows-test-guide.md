# Testing Veloren base building on Windows

Branch: `claude/veloren-base-building` of `jasonbenoit0501-source/veloren`.
Everything below is free. Plan for about **2 hours** the first time (mostly
waiting for the build).

**Your PC needs:** Windows 10 or 11 (64-bit), 16 GB RAM recommended (8 GB works
with the slow-build tip in Part C), **at least 40 GB free disk space**, and a
graphics card with up-to-date drivers.

---

## Part A — Install the tools (once)

1. **Git for Windows**: <https://git-scm.com/download/win>. Run the installer
   and keep all the default options.
2. **Visual Studio Build Tools 2022**:
   <https://visualstudio.microsoft.com/visual-cpp-build-tools/>. Run it, tick
   **"Desktop development with C++"** (this includes the C++ compiler and the
   Windows SDK), then click Install. This is about 6–8 GB. It's free for
   individuals and open-source work.
3. **Rust**: <https://rustup.rs>. Download `rustup-init.exe`, run it, and press
   Enter to accept the defaults. You don't need to pick a Rust version; the
   correct one, which Veloren pins in its `rust-toolchain` file, downloads by
   itself the first time you build.
4. **CMake, Ninja and Python** (needed to build the game's shader compiler). Open
   **PowerShell** (Start menu → type "PowerShell") and run:

   ```powershell
   winget install Kitware.CMake Ninja-build.Ninja Python.Python.3.12
   ```

   Accept any prompts. If `winget` isn't available, download them from
   <https://cmake.org/download/>, <https://github.com/ninja-build/ninja/releases>
   and <https://www.python.org/downloads/windows/>, and tick "Add to PATH" in
   each installer.
5. **Restart your PC**, then open a new PowerShell window and check that each of
   these prints a version number:

   ```powershell
   git --version
   cargo --version
   cmake --version
   ninja --version
   python --version
   ```

You do **not** need Git LFS. Your fork stores the game's images and sounds as
normal files.

---

## Part B — Download the development branch

Use a short folder path. Long paths can break the C++ part of the build.

```powershell
mkdir C:\dev
cd C:\dev
git clone --depth 1 --branch claude/veloren-base-building --single-branch https://github.com/jasonbenoit0501-source/veloren.git veloren
cd C:\dev\veloren
```

- `--depth 1` downloads only the latest code (about 0.8 GB), not years of
  history.
- If the repository is private, a GitHub sign-in window appears; sign in as
  usual.
- To get later changes: `cd C:\dev\veloren` and then `git pull`.
- Don't use GitHub's "Download ZIP" button. The build reads version information
  from Git and stops without it.

---

## Part C — Build the server and the game (once, 30–90 minutes)

```powershell
cd C:\dev\veloren
cargo build --bin veloren-server-cli --bin veloren-voxygen
```

It's finished when you see `Finished`. Warnings in yellow are fine. Red `error`
lines are not; copy them and send them to me.

- **PC freezes or "out of memory"**: build with fewer parallel jobs, which is
  slower but lighter: `cargo build -j 2 --bin veloren-server-cli --bin veloren-voxygen`
- **Optional speed-up:** Windows Security scans every file the build creates.
  Adding `C:\dev\veloren` as an exclusion (Windows Security → Virus & threat
  protection → Manage settings → Exclusions) can make builds much faster.

---

## Part D — Start the local server

1. In PowerShell:

   ```powershell
   cd C:\dev\veloren
   cargo run --bin veloren-server-cli -- --no-auth
   ```

   `--no-auth` means no Veloren account is needed: you can log in with any
   username and no password. The same username always counts as the same player.

2. The first start generates the world, which **can take several minutes**.
   Wait for the line **`Server is ready to accept connections`**. If Windows
   Firewall asks, click **Allow** (private networks).
3. Stop the server: click in its window, type `shutdown immediate` and press
   Enter. This saves everything. Avoid `Ctrl+C` or closing the window unless you
   mean to simulate a crash, because those may skip the final save.
4. **Turn on building saves.** Open
   `C:\dev\veloren\userdata\server\server_config\settings.ron` in Notepad. The
   file starts with `(`. Add this line straight after that first `(`:

   ```ron
       experimental_terrain_persistence: true,
   ```

   Save the file. Leave everything else as it is. Further down you'll also see
   `max_plots_per_player: 2,` and `plot_height: 128,` under `gameplay`; these are
   the plot limits, which you can change if you like.
5. Start the server again (step 1). When it's ready, the window should include
   lines like:
   - `Experimental terrain persistence support is enabled`
   - `Loaded saved build areas … areas=0`
   - `Loaded build ledger … blocks=0`

   If those lines are missing, the setting in step 4 wasn't saved correctly.
6. **Make an admin account** by typing this in the server window:

   ```
   admin add AdminTester admin
   ```

Leave the server running for all the tests.

---

## Part E — Start the game

1. Open a **second** PowerShell window:

   ```powershell
   cd C:\dev\veloren
   cargo run --bin veloren-voxygen
   ```

2. Click **Multiplayer**. Server address: `127.0.0.1`. Username: `Builder`. Leave
   the password empty. Connect, create a character and enter the world.
3. For the protection tests you need a second player. Either open a **third**
   PowerShell window and run the same command again (two game windows on one PC
   is heavy but usually works), or log out and back in with a different username.
   The test usernames used below are:
   - `Builder`: a normal player who builds.
   - `Intruder`: a normal player who shouldn't be able to touch Builder's plot.
   - `AdminTester`: the admin, who hands out items.

**Controls:** press **Enter** to open chat, type a command such as `/plot claim`,
and press Enter again. In build mode, **left-click** removes a block,
**right-click** places one, and **middle-click** copies the block you're looking
at as your building block. Type `/build` again to leave build mode before
fighting.

---

## Part F — Test checklist

Note what happens for each test. ✅ = expected result.

### 1. Plots
| # | Who | Do this | Expected |
|---|---|---|---|
| 1.1 | Builder | Walk well away from any town, type `/plot claim` | ✅ "You claimed plot plot_X_Y, from … to …" |
| 1.2 | Builder | `/plot list` | ✅ Shows the plot |
| 1.3 | Builder | Walk into a village and type `/plot claim` | ✅ Refused: too close to a settlement or dungeon |
| 1.4 | Builder | Claim a 2nd plot elsewhere, then try a 3rd | ✅ 2nd works, 3rd refused ("already own 2 plot(s)") |
| 1.5 | Intruder | Stand in Builder's plot, `/plot claim` | ✅ Refused ("already been claimed" / overlaps) |

### 2. Placing blocks and resource costs
| # | Who | Do this | Expected |
|---|---|---|---|
| 2.1 | Builder | In the plot: `/build`, then right-click the ground | ✅ "You need 1 Stones to place this block", nothing placed |
| 2.2 | AdminTester | `/sudo Builder give_item common.items.crafting_ing.stones 20` then `/sudo Builder give_item common.items.log.wood 20` | ✅ Builder receives items |
| 2.3 | Builder | Right-click the ground | ✅ White block placed, Stones go 20 → 19 |
| 2.4 | Builder | Middle-click a tree trunk, then right-click | ✅ Wood block placed, Wood Log 20 → 19 |
| 2.5 | Builder | Left-click that wood block | ✅ Block removed, Wood Log back to 20 |
| 2.6 | Builder | Left-click natural ground inside the plot | ✅ Removed, **no** item given |
| 2.7 | Builder | Right-click just outside the plot edge | ✅ Nothing happens |
| 2.8 | Builder | Fill the inventory (e.g. `/sudo Builder give_item` lots of different items), then left-click a block you placed | ✅ Block stays, "inventory is too full" message |
| 2.9 | AdminTester | `/permit_build world`, `/build`, place blocks inside Builder's plot | ✅ Placed for free (admins don't pay) |

### 3. Protection
| # | Who | Do this | Expected |
|---|---|---|---|
| 3.1 | Intruder | `/build` | ✅ "You do not have permission to build." |
| 3.2 | AdminTester | Stand inside Builder's plot, `/make_block WeakRock` | ✅ A rock block appears |
| 3.3 | AdminTester | `/sudo Intruder give_item common.items.tool.pickaxe_stone` | ✅ Intruder gets a pickaxe |
| 3.4 | Intruder | Equip the pickaxe and mine the weak rock in Builder's plot | ✅ Nothing breaks |
| 3.5 | Intruder | Mine weak rock *outside* any plot | ✅ Mining works as normal |
| 3.6 | AdminTester | `/sudo Intruder give_item common.items.utility.bomb 5` | ✅ Intruder gets bombs |
| 3.7 | Intruder | Throw a bomb at Builder's blocks | ✅ Blocks in the plot are not destroyed or scorched |
| 3.8 | Intruder | Throw a bomb at ground outside plots | ✅ Normal crater |

### 4. Trusting other players
| # | Who | Do this | Expected |
|---|---|---|---|
| 4.1 | Builder | Stand in the plot: `/plot trust Intruder` | ✅ Both get a message |
| 4.2 | Intruder | `/build`, place a block in Builder's plot (needs own Stones) | ✅ Works, Intruder pays |
| 4.3 | Builder | `/plot list` | ✅ Shows "trusted: Intruder" |
| 4.4 | Builder | `/plot untrust Intruder` | ✅ Intruder can no longer place or remove blocks there |

### 5. Saved buildings (do this last)
| # | Who | Do this | Expected |
|---|---|---|---|
| 5.1 | Builder | Build something recognisable, then **wait at least 70 seconds** | — |
| 5.2 | — | **Simulate a crash:** close the server window with the **X** button | — |
| 5.3 | — | Start the server again (Part D step 1) | ✅ Log shows `areas=` and `blocks=` above 0 |
| 5.4 | Builder | Log back in | ✅ Building is still there, plot still yours |
| 5.5 | Builder | `/build`, left-click a block placed before the restart | ✅ Material refunded |
| 5.6 | Intruder | Log in, try `/build` again (if still trusted from 4.1) | ✅ Trust survived the restart |
| 5.7 | — | Repeat with a clean stop: type `shutdown graceful 5` | ✅ Everything saved, even blocks placed in the last minute |

---

## Where the saves are, and how to reset

All in `C:\dev\veloren\userdata\server\`:

- `terrain\`: changed blocks
- `build_areas.ron`: plots, build areas and permissions
- `build_ledger.ron`: what each placed block cost (for refunds)
- `saves\`: characters and inventories

Copy the whole `userdata` folder to back it up. To start building tests from
scratch, stop the server and delete `terrain`, `build_areas.ron` and
`build_ledger.ron`.

## If something goes wrong

- **Build error mentioning `cmake`, `ninja` or `python`**: one of the Part A
  step 4 tools isn't installed or not on PATH. Restart the PC and check with
  `cmake --version` and so on.
- **`link.exe` not found**: Visual Studio Build Tools is missing the "Desktop
  development with C++" workload.
- **Game window black or crashing at start**: update your graphics drivers.
- **Can't connect**: make sure the server window says it's ready, and use
  address `127.0.0.1`.
- Send me the last 30 lines of any window that shows an error.
