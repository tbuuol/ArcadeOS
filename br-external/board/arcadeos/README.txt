ArcadeOS - SSH access and Wi-Fi
===============================

1) SSH key (required, otherwise nobody can log in via SSH)

  1. Create a file "authorized_keys.txt" in this folder.
  2. Paste your public key into it, one key per line, e.g.:
       ssh-ed25519 AAAAC3Nza... name@computer
     (contents of ~/.ssh/id_ed25519.pub - NEVER the private key!)

2) Wi-Fi (optional, otherwise use a network cable)

  Create a file "wifi.txt" in this folder:
       ssid=Name-of-the-network
       password=Wi-Fi-password
       country=US
  More networks: add further ssid=/password= lines below.
  Open network: leave password= empty. WPA2 (and WPA2/WPA3 mixed mode).

Then eject the stick safely and boot the computer from it.
The screen shows the IP address, the SSH command and the device's
host key fingerprint (compare it on first connect):

  ssh root@<IP>

Do not change anything else on this drive (EFI, a/, b/ = boot files).
