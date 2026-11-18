# Cloud storage

clipr can copy your clips to your own bucket on **Cloudflare R2**, **Backblaze B2**, **Amazon S3** or another
S3-compatible service. Your clips are then safe even if something happens to this PC, and Discord gets a link
to the full clip instead of a smaller copy.

There's no server of ours and no account with us: clipr talks to your bucket directly with your key. The files
on this PC stay where they are.

## Which service?

- **Cloudflare R2** (recommended): 10 GB free each month, and no fees when people watch your clips.
- **Backblaze B2:** 10 GB free, cheap storage after that.
- **Amazon S3:** you pay for storage and for every view.
- **Other:** any S3-compatible service, like MinIO or Wasabi.

## Set up Cloudflare R2

1. In the [Cloudflare dashboard](https://dash.cloudflare.com/), open **R2 Object Storage** and click **Create
   bucket**. Name it, for example `clipr-clips`.
2. Open the bucket's **Settings**. Under **Public access**, allow the **R2.dev subdomain** (or connect your own
   domain). Copy that address (`https://pub-….r2.dev`): it's the **public address**.
3. Back in R2, open **Manage API tokens** → **Create API token**. Pick **Object Read & Write**, apply it to
   **this bucket only**, and create it.
4. Copy the **Access Key ID**, the **Secret Access Key** and the endpoint
   (`https://<account id>.r2.cloudflarestorage.com`). Cloudflare shows the secret only once.
5. On the PC that runs clipr, open **Settings → Cloud**, pick **Cloudflare R2** and paste the endpoint, bucket
   name, key, secret and public address.
6. Click **Test**: clipr writes a small file, reads it back (also through the public address) and deletes it.
7. Turn on **Copy clips to the cloud** and click **Save**.

The steps for Backblaze B2 and Amazon S3 are under **How to set it up** in Settings → Cloud when you pick them.

Give the key access to this one bucket only. It's kept in clipr's settings file on this PC; other devices
(your phone) never see it and can't change the bucket.

## When clips go up

- **After the game** (default): new clips go up once the game closes, so uploading never costs frames.
- **Right away:** as soon as a clip is saved. Set a **speed limit** (Mbit/s) so games and voice chat stay
  smooth.
- **Only by hand:** only clips you upload from their page.

Full recordings only go up by themselves if you turn on **Upload recordings too**. One upload runs at a time;
big files go up in parts, so a dropped connection only repeats one part. An upload cut off by closing clipr
starts over the next time.

## In the gallery

A cloud icon on a clip shows its state: filled when it's in the cloud, with an arrow while it waits or uploads
(with the percentage), red when the upload failed. Home lists uploads on their way and failed ones, with
**Upload now**, **Cancel** and **Retry**.

On a clip's page, under **Cloud**:

- **Upload to the cloud** sends it right away, game or not.
- **Copy link** copies its public link (with a public address set).
- **Remove from cloud** deletes it from the bucket. Links to it stop working for everyone; the file on this PC
  stays.

Renaming a clip keeps its cloud copy and link. Trimming a clip in place uploads the new version under the same
name. Deleting a clip on this PC leaves its cloud copy.

Switching to another bucket (or endpoint) starts fresh: clips in the old bucket show no cloud icon and can go up
again. Switch back and they show as uploaded again.

## Discord

With cloud storage on and a public address set, a Discord post is a link to the clip in your bucket: Discord
shows a player, and the clip is full quality with no upload limit. If the clip isn't up yet when it's posted,
clipr uploads it first. Without a public address (or if the upload fails), Discord gets the file, made smaller
if it's over your upload limit, as before.

## Coming later

- **Cloud only:** delete the copy on this PC after it's safely uploaded.
- **Share pages** and an **online gallery** served from your bucket.
