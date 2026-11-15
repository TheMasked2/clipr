"""/agent: anyone allowed can run their own Pumpkin PC agent, so clips show *their* screen when they stream.

Pairing, without opening any ports: the agent makes an SSH key on their PC, they paste its public half into
/agent link, Pumpkin adds it to the server as a tunnel-only key (it can reach the agent port and nothing else)
and answers with a pairing code the agent asks for once.
"""

import os
from pathlib import Path

import discord
from discord import app_commands

AGENT_DIR = Path(__file__).resolve().parent.parent / "agent"
FILES = ["pumpkin_agent.py", "pumpkin-agent.cmd"]

SETUP = """🎥 **Your screen in clips** (Windows, while you Go Live)

1. Put the two files below in one folder (say `Documents\\Pumpkin`) and double-click **pumpkin-agent.cmd**. \
The first time it installs what it needs (uv and ffmpeg, through winget) and shows a line starting with `ssh-ed25519`.
2. Here, run `/agent link` and paste that line.
3. I answer with a pairing code. Paste it into the agent window. Done: it connects by itself from then on.

After that it runs hidden and starts by itself when you sign in to Windows, and it updates itself when I get a \
newer version, so you never touch it again. It only records while you're Go Live in Discord, keeps 2 minutes in a \
temp folder that overwrites itself, and only sends me the piece someone clips. Forget to go live a lot? \
`/agent incall` also records while I'm in your call; only you can clip your screen when you're not live. It never touches Discord itself \
(it's like OBS). `/agent autostart` turns starting with Windows off or on, `/agent stop` stops it until you \
double-click it again, `/agent unlink` throws the pairing away."""


START_AGAIN = "Double-click **pumpkin-agent.cmd** (wherever you put it) to start it again."
OWNER_START = "run `agent\\agent.ps1` in the repo (R:\\pumpkin), or sign out and in again"


def owner_note(bot) -> str:
    """The owner's agent runs from the repo with the token in .env: nothing to pair, only to start."""
    if bot.agent.connected(bot.settings.owner_id):
        return "🎥 you're the owner: your agent runs from the repo with the token in .env, and it's connected now."
    return f"🎥 you're the owner, so there's nothing to pair: your agent uses the token in .env. It isn't running " \
           f"right now: {OWNER_START}."


def add_agent_commands(tree: app_commands.CommandTree, bot) -> None:
    agent = app_commands.Group(name="agent", description="Your own PC agent, so clips show your screen")

    @agent.command(name="setup", description="How to get your screen into clips (the files and steps)")
    async def setup(interaction: discord.Interaction):
        if interaction.user.id == bot.settings.owner_id and bot.agent.owner_token:
            await interaction.response.send_message(owner_note(bot), ephemeral=True)
            return
        files = [discord.File(AGENT_DIR / name) for name in FILES if (AGENT_DIR / name).exists()]
        await interaction.response.send_message(SETUP, files=files, ephemeral=True)

    @agent.command(name="link", description="Pair your agent: paste the ssh-ed25519 line it shows")
    @app_commands.describe(public_key="The line starting with ssh-ed25519 from the agent window")
    async def link(interaction: discord.Interaction, public_key: str):
        server = os.environ.get("PUMPKIN_PUBLIC_SSH", "").strip()
        if not server:
            await interaction.response.send_message("pairing isn't set up on the server yet (the owner needs "
                                                    "PUMPKIN_PUBLIC_SSH in .env).", ephemeral=True)
            return
        if interaction.user.id == bot.settings.owner_id and bot.agent.owner_token:
            await interaction.response.send_message(owner_note(bot), ephemeral=True)
            return
        try:
            token = bot.agent.pair(interaction.user.id, public_key)
        except ValueError as e:
            await interaction.response.send_message(f"❌ {e}. Copy the whole line from the agent window.",
                                                    ephemeral=True)
            return
        except OSError as e:
            await interaction.response.send_message(f"❌ couldn't save the key on the server: {e}", ephemeral=True)
            return
        await interaction.response.send_message(
            "✅ paired. Paste this into the agent window (it's only shown once, and only to you):\n"
            f"```pumpkin:{server}:{token}```", ephemeral=True)

    @agent.command(name="unlink", description="Forget your agent's pairing (its key stops working)")
    async def unlink(interaction: discord.Interaction):
        gone = bot.agent.unpair(interaction.user.id)
        await interaction.response.send_message("🔌 unpaired." if gone else "you weren't paired.", ephemeral=True)

    @agent.command(name="status", description="Is your agent connected and recording?")
    async def status(interaction: discord.Interaction):
        uid = interaction.user.id
        if not bot.agent.paired(uid):
            text = "not paired. `/agent setup` shows how."
        elif not bot.agent.connected(uid):
            text = "paired, but the agent isn't running right now. " + START_AGAIN
        else:
            a = bot.agent.agents[uid]
            text = "connected, " + ("🔴 recording (you're live)." if bot.agent.recording(uid)
                                    else "waiting: it records when you Go Live.")
            text += ("\nAlso records while I'm in your call (`/agent incall`): yes." if
                     bot.personal.get(uid).get("agent_incall") else "")
            if a.autostart is None:
                text += "\nIt's an old version that can't update itself: get the new file from `/agent setup` once."
            else:
                text += f"\nStarts with Windows: {'yes' if a.autostart else 'no'}."
        await interaction.response.send_message(f"🎥 {text}", ephemeral=True)

    @agent.command(name="incall", description="Also record your screen whenever Pumpkin is in your call (not only "
                                              "when you Go Live)")
    async def incall(interaction: discord.Interaction, on: bool):
        uid = interaction.user.id
        data = bot.personal.get(uid)
        if on:
            data["agent_incall"] = True
        else:
            data.pop("agent_incall", None)
        bot.personal.save(uid)
        await bot.agent_sync(uid)
        if on:
            text = ("✅ your agent records your screen whenever I'm in your call too, live or not. when you're not "
                    "live, only you can clip it (*\"pumpkin, clip that\"*). `/agent incall on:False` undoes it.")
        else:
            text = "✅ back to recording only while you Go Live."
        if not bot.agent.paired(uid):
            text += "\n(you don't run the agent yet: `/agent setup`.)"
        await interaction.response.send_message(text, ephemeral=True)

    @agent.command(name="autostart", description="Should your agent start by itself when you sign in to Windows?")
    async def autostart(interaction: discord.Interaction, on: bool):
        if not await bot.agent.send(interaction.user.id, {"op": "autostart", "on": on}):
            await interaction.response.send_message("your agent isn't running, so I can't tell it. Double-click "
                                                    "pumpkin-agent.cmd first.", ephemeral=True)
            return
        if a := bot.agent.agents.get(interaction.user.id):
            a.autostart = on
        await interaction.response.send_message("✅ it starts with Windows now." if on else
                                                "✅ it won't start with Windows anymore (double-click it when you "
                                                "want it).", ephemeral=True)

    @agent.command(name="stop", description="Stop your agent on your PC (until you start it again)")
    async def stop(interaction: discord.Interaction):
        sent = await bot.agent.send(interaction.user.id, {"op": "quit"})
        await interaction.response.send_message("🛑 stopped. It comes back when you sign in to Windows (unless "
                                                "autostart is off) or when you double-click it." if sent else
                                                "it wasn't running.", ephemeral=True)

    tree.add_command(agent)
