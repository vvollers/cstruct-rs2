# Test artifacts

The artifacts used to test `cstruct-rs2`: disk images, reference target packs,
Cobalt Strike payloads and a packet capture, together with the Dissect functions
each one was exercised with.

Sizes are exact bytes as stored. For a split EWF image it is the sum of all
segments; the logical disk it exposes can be much larger than the stored files.

Function lists were produced with cstruct-rs2 against `dissect.target` revision
[`92568e19bba2e2bb8bf887c7521caa7ef57b0048`](https://github.com/fox-it/dissect.target/commit/92568e19bba2e2bb8bf887c7521caa7ef57b0048).
A function appears only where it emitted at least one record or yielded value.

## Disk images

Paths are relative to `artifacts/forensic-images/`. Files ending in `.001` are
raw images rather than EWF segments.

| Artifact and local input                                                           | Origin                                                                                                                                                                                                                                                                                                                                          |                                                                            Stored size | Contents                                                                                                                                                                                                                                                                                                           | Applicable Dissect analysis                                                                                                                                                                                |
| ---------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------: | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ashemery-challenge1-webserver/Case1-Webserver.E01`                                | Ali Hadi, [DFIR Challenge #1](https://www.ashemery.com/dfir.html); local copy from the [author-linked Internet Archive mirror](https://archive.org/details/dfir-case1)                                                                                                                                                                          |                                    3,124,542,874 bytes (2.91 GiB); 25 GiB logical disk | Windows Server 2008 SP1 web server, hostname `WIN-L0ZZQ76PMUF`, one NTFS filesystem, Apache logs, registry, event logs, and ordinary Windows system artifacts.                                                                                                                                                     | 67 `target-query` functions covering NTFS, MFT, registry, EVTX, DPAPI, Apache/web-server logs, LNK, network, and system state. [Exact list](#ashemery-challenge1-webserver).                               |
| `ashemery-challenge2-policy-violation/4orensics.001`                               | Ali Hadi, [DFIR Challenge #2](https://www.ashemery.com/dfir.html); local copy from the [author-linked Internet Archive mirror](https://archive.org/details/4orensics.case2.7z)                                                                                                                                                                  |                                                       26,474,446,848 bytes (24.66 GiB) | Raw Windows 8.1 Enterprise disk, hostname `4ORENSICS`, one NTFS filesystem. The scenario includes registry, USB, browser, email, remote-access, TeamViewer, MFT, and user-activity artifacts.                                                                                                                      | 93 `target-query` functions covering those Windows and application artifacts. [Exact list](#ashemery-challenge2-policy-violation).                                                                         |
| `ashemery-challenge3-win81/Windows8.1-Challenge3.001`                              | Ali Hadi, [DFIR Challenge #3](https://www.ashemery.com/dfir.html); local copy from the [author-linked Internet Archive mirror](https://archive.org/details/windows8.1challenge3.001.7z)                                                                                                                                                         |                                                       26,474,446,848 bytes (24.66 GiB) | Raw Windows 8.1 Enterprise disk, hostname `SENSEI`, one NTFS filesystem, from the “Mystery Hacked System” exercise.                                                                                                                                                                                                | 69 `target-query` functions covering Windows registry and system state, EVTX, MFT, LNK, Jump Lists, SRU, browser history, and user activity. [Exact list](#ashemery-challenge3-win81).                     |
| `cado-eks-2021/cado_cloud_collector_i-0630822f0d30a09ee_20GB_1625678779.dd`        | Cado Security, [AWS EKS Cluster Forensics v1.0](https://github.com/cado-security/AWS_EKS_Cluster_Forensics/releases/tag/v1.0); training use only, not for commercial use                                                                                                                                                                        | 21,474,836,480 bytes (20.00 GiB); retained source 7z is 1,171,071,221 bytes (1.09 GiB) | Compromised Amazon Linux 2 EKS node, hostname `ip-172-31-44-7`, with one XFS root filesystem, systemd journals, audit and authentication logs, SSH data, package logs, and eight discoverable Docker child targets.                                                                                                | 37 functions emit records across Linux host state, logs, SSH, Docker/container metadata, and filesystem traversal. [Exact list](#cado-eks-2021).                                                           |
| `dfirmadness-case001-dc01/20200918_0347_CDrive.E01` plus `.E02`                    | DFIR Madness, [The Stolen Szechuan Sauce](https://dfirmadness.com/the-stolen-szechuan-sauce/)                                                                                                                                                                                                                                                   |                                 4,872,431,197 bytes (4.54 GiB); 11.25 GiB logical disk | Windows Server 2012 R2 domain controller, hostname `CITADEL-DC01`, with two NTFS filesystems, Active Directory, registry, event logs, MFT, and server artifacts.                                                                                                                                                   | 79 `target-query` functions, including Active Directory computers, users, and group policies; Windows filesystem, registry, EVTX, DPAPI, RDP, and system plugins. [Exact list](#dfirmadness-case001-dc01). |
| `dfirmadness-case001-desktop/20200918_0417_DESKTOP-SDN1RPT.E01` plus `.E02`–`.E04` | DFIR Madness, [The Stolen Szechuan Sauce](https://dfirmadness.com/the-stolen-szechuan-sauce/)                                                                                                                                                                                                                                                   |                                    6,864,967,021 bytes (6.39 GiB); 15 GiB logical disk | Compromised Windows 10 Enterprise workstation, hostname `DESKTOP-SDN1RPT`, with one FAT and two NTFS filesystems. It includes registry, EVTX, MFT, SRU, Defender, browser, notification, and ETL data. Known ETL files include `NtfsLog.etl`, `NetCore.etl`, and `Wifi.etl` under `Windows/System32/LogFiles/WMI`. | 95 `target-query` functions, the broadest Windows surface in this inventory. [Exact list](#dfirmadness-case001-desktop). Individual ETL files can also be opened with `dissect.etl`.                       |
| `dftt-ext3/4-kwsrch-ext3/ext3-img-kw-1.dd`                                         | Brian Carrier, [Digital Forensics Tool Testing Test 4](http://dftt.sf.net), released under the GPL                                                                                                                                                                                                                                              |     5,242,880-byte image (5.00 MiB); retained source zip is 3,954,200 bytes (3.77 MiB) | Small raw ext3 filesystem made for keyword-search testing. It contains allocated, fragmented, deleted, journal-resident, and slack-space occurrences of four documented strings.                                                                                                                                   | Eight compatible functions; the useful input-facing functions are `walkfs` and `osinfo`. [Exact list](#dftt-ext3).                                                                                         |
| `m57-patents-jo-2009-11-20/jo-2009-11-20-newComputer.E01`                          | Digital Corpora, [M57 Patents scenario](https://digitalcorpora.org/corpora/scenarios/m57-patents-scenario/) and [direct E01](https://digitalcorpora.s3.amazonaws.com/corpora/scenarios/2009-m57-patents/drives-redacted/jo-2009-11-20-newComputer.E01); original scenario material is CC0, while embedded third-party files retain their rights |           2,357,780,960 bytes (2.20 GiB); 15,382,241,280-byte logical disk (14.33 GiB) | Windows XP SP3 workstation, hostname `M57-JO`, with one NTFS filesystem. It contains registry hives, legacy EVT logs, MFT and USN data, prefetch, LNK, shellbags, user activity, services, and Windows Update history.                                                                                             | 55 functions emit records across NTFS, registry, EVT, execution, network, user, and system-state plugins. [Exact list](#m57-patents-jo-2009-11-20).                                                        |
| `nist-dfir-ab/2020JimmyWilson.E01`                                                 | NIST CFReDS, [Forensics Image Test](https://cfreds.nist.gov/all/DFIR_AB/ForensicsImageTestimage)                                                                                                                                                                                                                                                |                          309,818,835 bytes (295.47 MiB); 891,289,600-byte logical disk | Windows 7 NTFS image, hostname `IACIS-HDD-2014`, with registry, EVTX, MFT, LNK, Jump Lists, shellbags, browser, USB, task, and other Windows artifacts. The publisher's case-study PDF is beside the image.                                                                                                        | 71 `target-query` functions covering the principal Windows forensic formats. [Exact list](#nist-dfir-ab).                                                                                                  |
| `nps-casper-rw/ubnist1.casper-rw.gen3.E01`                                         | Digital Corpora, [NPS 2009 Casper-RW generation 3](https://digitalcorpora.s3.us-west-2.amazonaws.com/corpora/drives/nps-2009-casper-rw/ubnist1.casper-rw.gen3.E01)                                                                                                                                                                              |                    168,365,166 bytes (160.57 MiB); 629,145,600-byte logical filesystem | ext3 persistence filesystem from an Ubuntu 8.10 bootable USB used over several weeks, including downloaded US government material, package state, logs, shell history, SSH data, and Firefox data.                                                                                                                 | 24 `target-query` functions covering ext3 traversal and the Linux, SSH, locate, Firefox, and log artifacts present. [Exact list](#nps-casper-rw).                                                          |
| `nps-ipod0/ipod_classic_160gb_virgin.E01`                                          | Digital Corpora, [NPS 2008 virgin iPod Classic](https://digitalcorpora.s3.us-west-2.amazonaws.com/corpora/drives/nps-2008-ipod0/ipod_classic_160gb_virgin.E01)                                                                                                                                                                                  |                           896,330,080 bytes (854.81 MiB); about 159.8 GB logical media | Factory-state Apple iPod Classic disk image. Dissect recognizes its FAT storage but does not identify a conventional host operating system.                                                                                                                                                                        | Eight generic and filesystem functions, principally `walkfs` and `osinfo`. [Exact list](#nps-ipod0).                                                                                                       |
| `nps-ubnist1/ubnist1.gen0.E01`                                                     | Digital Corpora, [NPS 2009 UBNIST1 generation 0](https://digitalcorpora.s3.us-west-2.amazonaws.com/corpora/drives/nps-2009-ubnist1/ubnist1.gen0.E01)                                                                                                                                                                                            |                        728,367,756 bytes (694.63 MiB); 2,106,589,184-byte logical disk | FAT32 image of a bootable Ubuntu 8.10 USB device. It is the full-device counterpart to the Casper-RW persistence image.                                                                                                                                                                                            | Eight generic and filesystem functions, principally `walkfs` and `osinfo`. [Exact list](#nps-ubnist1).                                                                                                     |
| `nps-xbox1/DRIVE2_TIME0.E01`                                                       | Digital Corpora, [NPS 2014 Xbox 360 DRIVE2 at time 0](https://digitalcorpora.s3.us-west-2.amazonaws.com/corpora/drives/nps-2014-xbox1/DRIVE2_TIME0.E01) and its [publisher narrative](https://digitalcorpora.s3.us-west-2.amazonaws.com/corpora/drives/nps-2014-xbox1/narrative.txt)                                                            |                           1,762,799,207 bytes (1.64 GiB); about 250.1 GB logical media | Xbox 360 drive populated through the normal console interface with accounts, Xbox Live activity, and game artifacts. The filesystem is XTAF, which `dissect.target` does not currently mount.                                                                                                                      | Seven generic functions can complete target probing, but `walkfs` and filesystem-content plugins are unavailable without XTAF support. [Exact list](#nps-xbox1).                                           |
| `ubuntu-defaced-2024/20240606-defaced_ecomm-Ubuntu_22.04.3.E01` plus `.E02`–`.E04` | Open University, [Defaced web server Ubuntu 22.04 simulation](https://ordo.open.ac.uk/articles/dataset/Defaced_web_server_Ubuntu_22_04_simulation_/26038669), CC BY-NC-SA 4.0                                                                                                                                                                   |             5,970,219,881 bytes (5.56 GiB); 1,073,741,824,000-byte sparse logical disk | Synthetic Ubuntu 22.04.3 e-commerce incident, hostname `shop`, with ext4 `/` and `/boot` inside LVM. It contains Apache, systemd journal, package, authentication, network, shell, SSH, service, and account artifacts.                                                                                            | 45 `target-query` functions covering ext4/LVM and the Linux and web-server artifacts present. [Exact list](#ubuntu-defaced-2024).                                                                          |

## Reference target packs

Paths are relative to `artifacts/reference-targets/`. These three generated ZIP
targets fill plugin families that the disk images cover sparsely. They are
deterministic fixtures rather than data from a real host.

| Artifact and local input         | Origin                                                                                                                                                                                                                                                                                                            |                                                                      Stored size | Contents                                                                                                                                                                                                                                                          | Applicable Dissect analysis                                                                                                                                                                                           |
| -------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------: | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `windows-reference-target.zip`   | Cado Security KAPE collection at revision [`3095b4b`](https://github.com/cado-security/kape-wannacry/commit/3095b4b6a5c7a5ee56f46dd8cdc79052ee1a0c59), plus browser fixtures from `dissect.target` revision [`92568e1`](https://github.com/fox-it/dissect.target/commit/92568e19bba2e2bb8bf887c7521caa7ef57b0048) | 63,858,325 bytes (60.90 MiB); 476,705,733 bytes (454.62 MiB) unpacked; 545 files | Windows Server 2022 KAPE collection with registry hives, MFT, EVTX, SRU, Defender and user data. Added profiles cover Brave, Chrome, Chromium, Edge, Firefox, and Opera. Treat the archive as potentially malicious because its base is a WannaCry investigation. | 103 functions emit output. Thirty were not covered by the scenario images: browser namespaces and passwords, `cit.modules`, and four SRU SDP providers. [Added functions](#windows-reference-target).                 |
| `linux-reference-target.zip`     | Files and layouts from `dissect.target` revision [`92568e1`](https://github.com/fox-it/dissect.target/commit/92568e19bba2e2bb8bf887c7521caa7ef57b0048)                                                                                                                                                            |            18,378 bytes (17.95 KiB); 65,289 bytes (63.76 KiB) unpacked; 43 files | Minimal Debian target with a procfs snapshot, socket tables, DHCP lease, kernel modules, and dpkg state and logs.                                                                                                                                                 | 23 functions emit output. Fourteen are new: processes, environment and command lines, sockets, modules, DHCP, netstat, and dpkg. [Added functions](#linux-reference-target).                                          |
| `container-reference-target.zip` | Docker and Podman fixtures from `dissect.target` revision [`92568e1`](https://github.com/fox-it/dissect.target/commit/92568e19bba2e2bb8bf887c7521caa7ef57b0048)                                                                                                                                                   |          33,742 bytes (32.95 KiB); 254,902 bytes (248.93 KiB) unpacked; 15 files | Minimal Linux host with Docker container configuration, image metadata, JSON/local logs, and Podman SQLite, image, configuration, and log state.                                                                                                                  | 15 functions emit output. The three Podman functions are not covered by the expanded scenario suite; the EKS disk now supplies independent Docker/container coverage. [Added functions](#container-reference-target). |

### windows-reference-target

`brave.cookies`, `brave.downloads`, `brave.extensions`, `brave.history`,
`brave.passwords`, `browser.passwords`, `chrome.extensions`,
`chrome.passwords`, `chromium.cookies`, `chromium.downloads`,
`chromium.extensions`, `chromium.history`, `chromium.passwords`, `cit.modules`,
`edge.cookies`, `edge.downloads`, `edge.extensions`, `edge.history`,
`edge.passwords`, `firefox.downloads`, `firefox.passwords`, `opera.cookies`,
`opera.downloads`, `opera.extensions`, `opera.history`, `opera.passwords`,
`sru.sdp_cpu_provider`, `sru.sdp_network_provider`,
`sru.sdp_physical_disk_provider`, `sru.sdp_volume_provider`

### linux-reference-target

`cmdline`, `dpkg.logs`, `dpkg.packages`, `environ`, `lsmod`, `netstat`,
`network.dhcp`, `processes`, `sockets.packet`, `sockets.raw`, `sockets.tcp`,
`sockets.udp`, `sockets.unix`, `sysmodules`

### container-reference-target

`podman.containers`, `podman.images`, `podman.logs`

## Cobalt Strike payloads

Seven encrypted zip archives from the MIT-licensed
[`dissect.cobaltstrike` test fixtures](https://github.com/fox-it/dissect.cobaltstrike/tree/c53faed03f17d6e3dcf852939c1ef5ad773b7074/tests/beacons),
pinned to revision `c53faed03f17d6e3dcf852939c1ef5ad773b7074`. The archive
password is `dissect.cobaltstrike`. Each archive contains one real Beacon
payload and must be treated as malicious. They are not `dissect.target`
targets.

| Artifact and local archive                                                              |            Stored zip size |             Payload member | Contents                                                     | Applicable Dissect tools                                                                            |
| --------------------------------------------------------------------------------------- | -------------------------: | -------------------------: | ------------------------------------------------------------ | --------------------------------------------------------------------------------------------------- |
| `ecosystem/dissect.cobaltstrike/tests/beacons/124552cf674b362e0c916ab79b9e7a56.bin.zip` | 179,824 bytes (175.61 KiB) | 342,528 bytes (334.50 KiB) | Real Guardrails-protected Beacon.                            | `beacon-dump`; `c2profile-dump --beacon`; `BeaconConfig.from_bytes()`; Beacon Gate option decoding. |
| `ecosystem/dissect.cobaltstrike/tests/beacons/1897a6cdf17271807bd6ec7c60fffea3.bin.zip` | 224,520 bytes (219.26 KiB) | 262,735 bytes (256.58 KiB) | Real x64 Beacon.                                             | `beacon-dump`; `c2profile-dump --beacon`; `BeaconConfig.from_bytes()`; Beacon Gate option decoding. |
| `ecosystem/dissect.cobaltstrike/tests/beacons/37882262c9b5e971067fd989b26afe28.bin.zip` | 183,189 bytes (178.90 KiB) | 208,452 bytes (203.57 KiB) | Real Beacon used by `dissect.cobaltstrike` C2 parsing tests. | `beacon-dump`; `c2profile-dump --beacon`; `BeaconConfig.from_bytes()`; Beacon Gate option decoding. |
| `ecosystem/dissect.cobaltstrike/tests/beacons/3fdf92571d10485b05904e35c635c655.bin.zip` | 187,480 bytes (183.09 KiB) | 211,035 bytes (206.09 KiB) | Real Beacon with a custom XOR key.                           | `beacon-dump`; `c2profile-dump --beacon`; `BeaconConfig.from_bytes()`; Beacon Gate option decoding. |
| `ecosystem/dissect.cobaltstrike/tests/beacons/4f571c0bc97c20eefc58fa3faf32148d.bin.zip` | 186,372 bytes (182.00 KiB) | 208,472 bytes (203.59 KiB) | Real x86 Beacon.                                             | `beacon-dump`; `c2profile-dump --beacon`; `BeaconConfig.from_bytes()`; Beacon Gate option decoding. |
| `ecosystem/dissect.cobaltstrike/tests/beacons/5a197a8bb628a2555f5a86c51b85abd7.bin.zip` | 183,924 bytes (179.61 KiB) | 208,466 bytes (203.58 KiB) | Real Beacon configured with a non-ASCII domain.              | `beacon-dump`; `c2profile-dump --beacon`; `BeaconConfig.from_bytes()`; Beacon Gate option decoding. |
| `ecosystem/dissect.cobaltstrike/tests/beacons/a1573fe60c863ed40fffe54d377b393a.bin.zip` | 112,165 bytes (109.54 KiB) | 209,413 bytes (204.50 KiB) | Real DNS Beacon.                                             | `beacon-dump`; `c2profile-dump --beacon`; `BeaconConfig.from_bytes()`; Beacon Gate option decoding. |

## Packet capture

| Artifact and local archive                                                                                 | Origin                                                                                                                                                               |                                                              Stored size | Contents                                                                                                                                              | Applicable Dissect tools                                                                                                                                                                                                                                                       |
| ---------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -----------------------------------------------------------------------: | ----------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `cstruct-rs2/artifacts/forensic-inputs/2021-06-15-Hancitor-with-Ficker-Stealer-and-Cobalt-Strike.pcap.zip` | [Malware-Traffic-Analysis.net, 15 June 2021](https://www.malware-traffic-analysis.net/2021/06/15/2021-06-15-Hancitor-with-Ficker-Stealer-and-Cobalt-Strike.pcap.zip) | 1,503,650-byte zip (1.43 MiB); one 4,325,317-byte PCAP member (4.13 MiB) | Malware traffic involving Hancitor, Ficker Stealer, and Cobalt Strike, including staged Beacon payloads. The archive password is `infected_20210615`. | `beacon-pcap`, backed by `BeaconCapture.iter_parse_pcap()` and `c2packet_to_record()`. It requires the `dissect.cobaltstrike` PCAP dependencies, PyShark, and TShark. The tool takes the decrypted PCAP member, not the zip; this is not a `dissect.target` disk-image plugin. |

## Function lists per artifact

The lists include Dissect's built-in example and empty-key-provider functions
where they are compatible.

### ashemery-challenge1-webserver

`alternateshell`, `apache.access`, `apache.error`, `apache.logs`, `appinit`,
`applications`, `auditpol`, `catroot.files`, `clsid.machine`,
`credential.credhist`, `credential.defaultpassword`, `credhist.credhist`,
`defaultpassword.defaultpassword`, `dpapi.keyprovider.defaultpassword.lsa.keys`,
`dpapi.keyprovider.empty.keys`, `dpapi.keyprovider.keys`,
`environment_variables`, `evtx`, `example`,
`example_namespace.example_record`, `example_record`,
`example_user_registry_record`, `example_yield`, `firewall.rules`, `keyboard`,
`knowndlls`, `license`, `lnk`, `lsa.secrets`, `machine_sid`, `mft.body`,
`mft.records`, `mft.timeline`, `mft_timeline`, `mru.lastvisited`,
`mru.opensave`, `muicache`, `ndis`, `network.dns`, `network.gateways`,
`network.interfaces`, `network.ips`, `network_history`, `nullsessionpipes`,
`osinfo`, `path_extensions`, `pathenvironment`, `productkey`, `regf`,
`runkeys`, `sam`, `schedlgu`, `scraped_evtx`, `services`, `sessionmanager`,
`shellbags`, `shimcache`, `sid`, `tasks`, `userassist`, `usnjrnl`, `walkfs`,
`webserver.access`, `webserver.error`, `webserver.logs`, `wer`,
`winsocknamespaceprovider`

### ashemery-challenge2-policy-violation

`alternateshell`, `amcache.files`, `amcache.programs`, `appinit`,
`applications`, `auditpol`, `bootshell`, `browser.cookies`,
`browser.downloads`, `browser.history`, `catroot.catdb`, `catroot.files`,
`chrome.cookies`, `chrome.downloads`, `chrome.history`, `clfs`, `clsid.machine`,
`clsid.user`, `credential.defaultpassword`, `defaultpassword.defaultpassword`,
`defender.evtx`, `defender.mpcmdrun`, `defender.mplog`,
`dpapi.keyprovider.defaultpassword.lsa.keys`, `dpapi.keyprovider.empty.keys`,
`dpapi.keyprovider.keys`, `environment_variables`, `evtx`, `example`,
`example_namespace.example_record`, `example_record`,
`example_user_registry_record`, `example_yield`, `filerenameop`,
`firewall.rules`, `iexplore.downloads`, `iexplore.history`,
`jumplist.automatic_destination`, `jumplist.custom_destination`, `keyboard`,
`knowndlls`, `license`, `lnk`, `lsa.secrets`, `machine_sid`, `mft.body`,
`mft.records`, `mft.timeline`, `mft_timeline`, `mru.lastvisited`,
`mru.msoffice`, `mru.opensave`, `mru.recentdocs`, `msoffice.native`,
`msoffice.startup`, `muicache`, `ndis`, `network.dns`, `network.gateways`,
`network.interfaces`, `network.ips`, `network_history`, `osinfo`,
`path_extensions`, `pathenvironment`, `productkey`, `regf`,
`remoteaccess.incoming`, `remoteaccess.logs`, `runkeys`, `sam`, `scraped_evtx`,
`search`, `services`, `sessionmanager`, `shellbags`, `shimcache`, `sid`,
`sru.application`, `sru.network_connectivity`, `sru.network_data`,
`sru.push_notification`, `startupinfo`, `tasks`, `teamviewer.incoming`,
`teamviewer.logs`, `usb`, `userassist`, `usnjrnl`, `walkfs`, `wer`,
`winsocknamespaceprovider`, `wua_history`

### ashemery-challenge3-win81

`alternateshell`, `amcache.files`, `amcache.programs`, `appinit`,
`applications`, `auditpol`, `bootshell`, `browser.history`, `catroot.catdb`,
`catroot.files`, `clsid.machine`, `credential.defaultpassword`,
`defaultpassword.defaultpassword`, `defender.evtx`,
`dpapi.keyprovider.defaultpassword.lsa.keys`, `dpapi.keyprovider.empty.keys`,
`dpapi.keyprovider.keys`, `environment_variables`, `evtx`, `example`,
`example_namespace.example_record`, `example_record`,
`example_user_registry_record`, `example_yield`, `filerenameop`,
`firewall.rules`, `iexplore.history`, `jumplist.automatic_destination`,
`keyboard`, `knowndlls`, `license`, `lnk`, `lsa.secrets`, `machine_sid`,
`mft.body`, `mft.records`, `mft.timeline`, `mft_timeline`, `mru.recentdocs`,
`msoffice.native`, `muicache`, `ndis`, `network.interfaces`, `network.ips`,
`osinfo`, `path_extensions`, `pathenvironment`, `pfro`, `productkey`, `regf`,
`runkeys`, `sam`, `scraped_evtx`, `search`, `services`, `sessionmanager`,
`shellbags`, `shimcache`, `sid`, `sru.application`,
`sru.network_connectivity`, `sru.network_data`, `startupinfo`, `tasks`,
`userassist`, `usnjrnl`, `walkfs`, `wer`, `winsocknamespaceprovider`

### cado-eks-2021

`audit`, `authlog`, `bashhistory`, `btmp`, `commandhistory`,
`container.containers`, `container.images`, `container.logs`, `cronjobs`,
`docker.containers`, `docker.images`, `docker.logs`, `example`,
`example_namespace.example_record`, `example_record`, `example_yield`,
`journal`, `lastlog`, `messages`, `openssh.authorized_keys`,
`openssh.private_keys`, `openssh.public_keys`, `opensshd.config`, `osinfo`,
`packagemanager.logs`, `securelog`, `services`, `ssh.authorized_keys`,
`ssh.config`, `ssh.private_keys`, `ssh.public_keys`, `suid_binaries`, `syslog`,
`utmp`, `walkfs`, `wtmp`, `yum.logs`

### dfirmadness-case001-dc01

`ad.computers`, `ad.group_policies`, `ad.users`, `adpolicy`, `alternateshell`,
`amcache.files`, `amcache.programs`, `appinit`, `applications`, `auditpol`,
`bootshell`, `browser.downloads`, `browser.history`, `catroot.catdb`,
`catroot.files`, `clfs`, `clsid.machine`, `credential.defaultpassword`,
`defaultpassword.defaultpassword`, `dpapi.keyprovider.defaultpassword.lsa.keys`,
`dpapi.keyprovider.empty.keys`, `dpapi.keyprovider.keys`,
`environment_variables`, `evtx`, `example`,
`example_namespace.example_record`, `example_record`,
`example_user_registry_record`, `example_yield`, `firewall.rules`,
`iexplore.downloads`, `iexplore.history`, `jumplist.automatic_destination`,
`jumplist.custom_destination`, `keyboard`, `knowndlls`, `license`, `lnk`,
`lsa.secrets`, `machine_sid`, `mft.body`, `mft.records`, `mft.timeline`,
`mft_timeline`, `mru.lastvisited`, `mru.mstsc`, `mru.opensave`,
`mru.recentdocs`, `muicache`, `ndis`, `network.dns`, `network.gateways`,
`network.interfaces`, `network.ips`, `network_history`, `osinfo`,
`path_extensions`, `pathenvironment`, `pfro`, `productkey`, `rdpcache.paths`,
`recyclebin`, `regf`, `runkeys`, `sam`, `scraped_evtx`, `services`,
`sessionmanager`, `shellbags`, `shimcache`, `sid`, `tasks`,
`ual.system_identities`, `userassist`, `usnjrnl`, `walkfs`, `wer`,
`winsocknamespaceprovider`, `wua_history`

### dfirmadness-case001-desktop

`activitiescache`, `adpolicy`, `alternateshell`,
`amcache.application_files`, `amcache.applications`,
`amcache.device_containers`, `amcache.drivers`, `amcache.shortcuts`, `appinit`,
`applications`, `auditpol`, `bam`, `bootshell`, `browser.downloads`,
`browser.history`, `cam.registry`, `catroot.catdb`, `catroot.files`, `cit.dp`,
`cit.puu`, `clfs`, `clsid.machine`, `clsid.user`, `defender.evtx`,
`defender.mpcmdrun`, `defender.mplog`, `domain_sid`,
`dpapi.keyprovider.empty.keys`, `dpapi.keyprovider.keys`,
`environment_variables`, `evtx`, `example`,
`example_namespace.example_record`, `example_record`,
`example_user_registry_record`, `example_yield`, `filerenameop`,
`firewall.rules`, `iexplore.downloads`, `iexplore.history`,
`jumplist.automatic_destination`, `jumplist.custom_destination`, `keyboard`,
`knowndlls`, `license`, `lnk`, `lsa.secrets`, `machine_sid`, `mft.body`,
`mft.records`, `mft.timeline`, `mft_timeline`, `mru.lastvisited`,
`mru.networkdrive`, `mru.opensave`, `mru.recentdocs`, `muicache`, `ndis`,
`network.dns`, `network.gateways`, `network.interfaces`, `network.ips`,
`network_history`, `notifications.wpndatabase`, `osinfo`, `path_extensions`,
`pathenvironment`, `prefetch`, `productkey`, `regf`, `runkeys`, `sam`,
`scraped_evtx`, `search`, `services`, `sessionmanager`, `shellbags`,
`shimcache`, `sid`, `sru.application`, `sru.application_timeline`,
`sru.network_connectivity`, `sru.network_data`, `sru.push_notification`,
`sru.vfu`, `startupinfo`, `tasks`, `thumbcache.iconcache`,
`thumbcache.thumbcache`, `userassist`, `usnjrnl`, `walkfs`, `wer`,
`winsocknamespaceprovider`, `wua_history`

### dftt-ext3

`dpapi.keyprovider.empty.keys`, `dpapi.keyprovider.keys`, `example`,
`example_namespace.example_record`, `example_record`, `example_yield`, `osinfo`,
`walkfs`

### m57-patents-jo-2009-11-20

`alternateshell`, `appinit`, `applications`, `catroot.files`, `clsid.machine`,
`commandprocautorun`, `dpapi.keyprovider.empty.keys`,
`dpapi.keyprovider.keys`, `environment_variables`, `evt`, `example`,
`example_namespace.example_record`, `example_record`,
`example_user_registry_record`, `example_yield`, `keyboard`, `knowndlls`,
`license`, `lnk`, `lsa.secrets`, `machine_sid`, `mft.body`, `mft.records`,
`mft.timeline`, `mft_timeline`, `mru.lastvisited`, `mru.networkdrive`,
`mru.opensave`, `mru.run`, `muicache`, `ndis`, `network.dns`,
`network.gateways`, `network.interfaces`, `network.ips`, `nullsessionpipes`,
`osinfo`, `path_extensions`, `pathenvironment`, `prefetch`, `productkey`,
`regf`, `runkeys`, `sam`, `schedlgu`, `scraped_evt`, `services`,
`sessionmanager`, `shellbags`, `sid`, `userassist`, `usnjrnl`, `walkfs`,
`winsocknamespaceprovider`, `wua_history`

### nist-dfir-ab

`alternateshell`, `appinit`, `applications`, `auditpol`, `browser.cookies`,
`browser.extensions`, `browser.history`, `catroot.files`, `clsid.machine`,
`credential.defaultpassword`, `defaultpassword.defaultpassword`,
`defender.evtx`, `dpapi.keyprovider.defaultpassword.lsa.keys`,
`dpapi.keyprovider.empty.keys`, `dpapi.keyprovider.keys`,
`environment_variables`, `evtx`, `example`,
`example_namespace.example_record`, `example_record`,
`example_user_registry_record`, `example_yield`, `firefox.cookies`,
`firefox.extensions`, `firefox.history`, `firewall.rules`, `iexplore.history`,
`jumplist.automatic_destination`, `jumplist.custom_destination`, `keyboard`,
`knowndlls`, `license`, `lnk`, `lsa.secrets`, `machine_sid`, `mft.body`,
`mft.records`, `mft.timeline`, `mft_timeline`, `mru.lastvisited`,
`mru.opensave`, `mru.recentdocs`, `msoffice.native`, `muicache`, `ndis`,
`network.dns`, `network.gateways`, `network.interfaces`, `network.ips`,
`network_history`, `osinfo`, `path_extensions`, `pathenvironment`, `prefetch`,
`productkey`, `recyclebin`, `regf`, `runkeys`, `sam`, `scraped_evtx`,
`services`, `sessionmanager`, `shellbags`, `shimcache`, `sid`, `tasks`,
`thumbcache.thumbcache`, `usb`, `userassist`, `walkfs`,
`winsocknamespaceprovider`

### nps-casper-rw

`applications`, `authlog`, `bashhistory`, `browser.history`, `commandhistory`,
`dpkg.log`, `dpkg.status`, `example`, `example_namespace.example_record`,
`example_record`, `example_yield`, `firefox.history`, `locate.locate`,
`messages`, `mlocate.locate`, `openssh.known_hosts`, `osinfo`, `securelog`,
`services`, `ssh.known_hosts`, `syslog`, `utmp`, `walkfs`, `wtmp`

### nps-ipod0

`dpapi.keyprovider.empty.keys`, `dpapi.keyprovider.keys`, `example`,
`example_namespace.example_record`, `example_record`, `example_yield`, `osinfo`,
`walkfs`

### nps-ubnist1

`dpapi.keyprovider.empty.keys`, `dpapi.keyprovider.keys`, `example`,
`example_namespace.example_record`, `example_record`, `example_yield`, `osinfo`,
`walkfs`

### nps-xbox1

`dpapi.keyprovider.empty.keys`, `dpapi.keyprovider.keys`, `example`,
`example_namespace.example_record`, `example_record`, `example_yield`, `osinfo`

### ubuntu-defaced-2024

`apache.access`, `apache.error`, `apache.hosts`, `apache.logs`, `applications`,
`apt.logs`, `authlog`, `bashhistory`, `commandhistory`, `cronjobs`, `dpkg.log`,
`dpkg.status`, `example`, `example_namespace.example_record`, `example_record`,
`example_yield`, `iptables`, `journal`, `keyboard`, `lastlog`, `messages`,
`network.interfaces`, `network.ips`, `openssh.private_keys`,
`openssh.public_keys`, `opensshd.config`, `osinfo`, `packagemanager.logs`,
`passwords`, `securelog`, `services`, `snap`, `snaps`, `ssh.config`,
`ssh.private_keys`, `ssh.public_keys`, `suid_binaries`, `syslog`, `utmp`,
`walkfs`, `webserver.access`, `webserver.error`, `webserver.hosts`,
`webserver.logs`, `wtmp`

## Handling

- Keep the disk images and all malware-related inputs read-only. Never execute
  files extracted from the Beacon archives or recovered from the PCAP.
- The DFIR Madness images are intentionally compromised hosts. The Cobalt Strike
  fixtures contain real Beacon payloads, and the PCAP contains malware traffic
  with embedded staged payloads.
- The large images are not redistributable by default. The source terms beside
  each image govern copying it out of this workspace.
