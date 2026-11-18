# Discord

clipr can post your clips to Discord channels. Each post is one message with the clip attached and a small
card showing the title, game, length and a thumbnail. It works through **webhooks**: a channel's own posting
address. No bot, no server of ours and no Discord login are needed.

Nothing is posted until you add a channel, and by default clipr asks before every post.

## Add a channel

You need the **Manage Webhooks** permission on the server (you have it on your own server).

1. In Discord, hover over the channel and click the gear (**Edit Channel**).
2. Open **Integrations → Webhooks → New Webhook**. Optionally rename it (for example "clipr") and give it a
   picture.
3. Click **Copy Webhook URL**.
4. On the PC that runs clipr, open **Settings → Discord** and click **Add a channel**.
5. Give it a name (for example `#clips`) and paste the webhook URL.
6. Click **Test**: a message "clipr is connected" should appear in the channel. Then click **Save**.

Keep the webhook URL to yourself: anyone who has it can post to that channel. clipr never writes it to its logs,
and for that reason a webhook URL can only be pasted or changed on the PC itself, not from your phone.

## When clipr posts

You can add up to 10 channels. Each channel has its own choice for new clips:

- **Ask** (default): new clips wait on the **Home** page. Click **Post** or **Skip** for each.
- **Auto:** new clips are posted by themselves.
- **Off:** new clips aren't posted. You can still share a clip by hand.

Per channel you can also limit posting to **only these games** (names as clipr shows them in the gallery, like
`VALORANT`). Empty means every game. That way each game can have its own channel.

Two settings apply to all channels:

- **Post while a game runs** (off): when off, posts wait until the game closes, so uploading never costs you
  frames. Clips you share by hand go right away.
- **Upload limit** (10 MB): your Discord upload limit: 10 MB on a free account, 50 MB with Nitro Basic, 500 MB
  with Nitro. A clip bigger than that is sent as a smaller copy that fits. The clip itself never changes.

Full recordings are never posted by themselves. To share one, use **Share to Discord** on its page.

## Share a clip by hand

Open the clip in the Gallery. Under **Share to Discord**, pick a channel and click **Post**: it's posted right
away, game or not. This works with any channel mode, Off included.

The Home page lists recent posts with their state: waiting for you, waiting for the game to close, posting,
posted, skipped or failed. A failed post has a **Retry** button.

## Your Discord status

While clipr clips or records, your Discord profile shows the game with its icon: *Playing clipr · VALORANT ·
Clipping with clipr*. It's on by default; turn it off with **Show in my Discord status** in Settings → Discord.
It talks to the Discord app on this PC, so there's nothing to sign in to.

## Coming later

**Connect Discord:** sign in through Discord to pick a channel in Discord's own picker instead of pasting a
webhook URL. clipr has no bot: webhooks post, and clipr itself asks before posting.
