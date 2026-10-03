#!/usr/bin/env node
// CP1252-specific probe construction; bounded exchange/retention primitives
// reuse the owner-reviewed task895 capture and task899 envelopes (owner
// PR902 checkpoint edbdd67a), immutable at their pinned source hashes.
import assert from 'node:assert/strict'
import {mkdir, readFile, writeFile} from 'node:fs/promises'
import {dirname, resolve} from 'node:path'
import {fileURLToPath} from 'node:url'
import {createHash} from 'node:crypto'
import {createRequire} from 'node:module'
import {isDeepStrictEqual} from 'node:util'
import {TYPES} from 'tedious'
import {capture} from './lib/compatibility.mjs'
import {connect, isolatedReference} from './lib/reference.mjs'
import {withReferenceContainer, referenceImage} from './lib/reference-container.mjs'
import {CAPTURE_LIMIT, profiles, jsonSize, budget, guardOutput, exchange, compare, responseSignature, readCaptureFile} from './capture-bulk-character-conversion.mjs'
export {CAPTURE_LIMIT, jsonSize, budget, guardOutput, exchange, compare, readCaptureFile}
const require = createRequire(import.meta.url)
const {Collation} = require('tedious/lib/collation')
const CLIENT_VERSION = require('tedious/package.json').version
const SOURCE = fileURLToPath(import.meta.url)
const SOURCE_SHA = createHash('sha256').update(await readFile(SOURCE)).digest('hex')
const HELPER_SHA = createHash('sha256').update(await readFile(new URL('./capture-bulk-character-conversion.mjs', import.meta.url))).digest('hex')
const EXPECTED_HELPERS = {"capture-bulk-character-conversion.mjs":"9dd773a8170ea2c255d51bf40574fa75474563902c9211a2bceae7f5382e8868","lib/compatibility.mjs":"62f032fce72cf9a1fa0cb14bb2c7f1b5a2c396fdf4e8fcd5486d8db96aa0cf4d","lib/reference.mjs":"419a96611c0251e66b1f0783c9413151b4dacb5ee005ad89383402ff8c738df8","lib/reference-container.mjs":"d5b8d66c920330cbb4554cd16f1525f005eed9e3bdc69d5b00e421791545120c","capture-bulk-character-capacity.mjs":"2cbb4172e902e2a4817812de570d09e375533da8a3efaf539cd08f9408975e87"}
const HELPERS = Object.fromEntries(await Promise.all(Object.keys(EXPECTED_HELPERS).map(async name => [name,createHash('sha256').update(await readFile(new URL(name,import.meta.url))).digest('hex')])))
const EXPECTED_HELPER_SHA = '9dd773a8170ea2c255d51bf40574fa75474563902c9211a2bceae7f5382e8868'
const fixture = fileURLToPath(new URL('../reference/bulk-character-cp1252.json', import.meta.url))
const LIMIT = 2 * 1024 * 1024
const MAX_PACKETS = 1024
const GOLD = {
  "sourceSha": "1f5efb789cc7b3332a906de50e2c44fc0dc38b1ec2600ef3dd2b3a3a62cb5960",
  "nodeVersion": "24.13.0",
  "version": "1509703d2c117014f2615f5ff9fcdd4775a2e1a1db43be28520ccd71e9f919fd",
  "versionRequest": "3f611a97ed172bd33f68a477c73486bd09fb937d9d6cdd2e5783d763a434631f",
  "versionResponse": "fbb3249ff9242ae6fe494716c62f9ac3a6395dcfa7f152dfbbd1529e38dc3e68",
  "semantics": {
    "cp1252-every-byte-to-cp1252": "369375a28d98b9b35f34f5305448c4b7a4ae860d050fde10ecb89c2300e9ba31",
    "cp1252-mixed-to-cp1252": "5589ece1db5c3a36bfb39c4d7e1a61e5dd290bef50fe1f5381d20ef685434e30",
    "cp1252-fragmented-to-cp1252": "1fb9d1979c09af560b6f1cbb4c171adafa479cca4078be790b31c429f4a8e32a",
    "cp1252-every-byte-to-cp1251": "e63447d6500a3293328d759709732124b34f7630f10baa41534496ad200f67e8",
    "cp1252-mixed-to-cp1251": "b18a863a6ca38c88d91aca7255e7baca3de48550725b4820876b972d4e8c0fcd",
    "cp1252-fragmented-to-cp1251": "b3429c9cce313bad698caa3053c58d9e416a5f77262095a2a3022cd7029a7daa",
    "cp1252-every-byte-to-utf8": "c4b5dd23980bad8c1101c618a37d109a92d53b5722e99be30e99345c9d8d317d",
    "cp1252-mixed-to-utf8": "da55c7c57432fc261894e06a7a7d68e34eb8fe80f0b2d5fb4d46b3296cd0683b",
    "cp1252-fragmented-to-utf8": "283f64dc153ceff45f940aaa31fe0694e99fa10023478f6b1a6fbcf6ec577701",
    "cp1252-every-byte-to-unicode": "e99c87f38ac3a29775869e21bbadc2353d339b2b92b103a4920389ab5044ba69",
    "cp1252-mixed-to-unicode": "418b8e4fc0de14c9d8ce5447912f6ea9293c6ba592f9784a74cd74d563fdb8ee",
    "cp1252-fragmented-to-unicode": "344006abebd58080ba1b8f7efcc6e01233db2bc7e7effee8936f2f3a01cee6b2"
  },
  "requests": {
    "cp1252-every-byte-to-cp1252": {
      "metadata": "46240a0e434fe68446de2ad8ec5e4fc0ead9de062e3fb4fe9daec17752ed4355",
      "setup": "da26af8ea8ea4521aec227cd3458ed49734cfd426228e53a20f537cdd75d3cb6",
      "execution": "838071afba8bdaed8b770df5e65bd50489b66c8569d0a23aa8b5131b3473c5aa",
      "readback": "4d98362b6ab5c7ee88cf16fbf2ea0b0a16d99325dfe34faeba7dddcc54faaf74",
      "cleanup": "7e5ad676582b99099e0a19de746ace8b68b5312ff30c4600809da90378e73384"
    },
    "cp1252-mixed-to-cp1252": {
      "metadata": "46240a0e434fe68446de2ad8ec5e4fc0ead9de062e3fb4fe9daec17752ed4355",
      "setup": "da26af8ea8ea4521aec227cd3458ed49734cfd426228e53a20f537cdd75d3cb6",
      "execution": "d151ced1e97b099d99ad53d0fcacaddd6d25d50e3d775496a726f6eadfd1ab7d",
      "readback": "4d98362b6ab5c7ee88cf16fbf2ea0b0a16d99325dfe34faeba7dddcc54faaf74",
      "cleanup": "7e5ad676582b99099e0a19de746ace8b68b5312ff30c4600809da90378e73384"
    },
    "cp1252-fragmented-to-cp1252": {
      "metadata": "46240a0e434fe68446de2ad8ec5e4fc0ead9de062e3fb4fe9daec17752ed4355",
      "setup": "da26af8ea8ea4521aec227cd3458ed49734cfd426228e53a20f537cdd75d3cb6",
      "execution": "dd81c2fa3cc0285373d8b8c6969a206fbb8aba78cc9de98b42bf295d1d08d5b6",
      "readback": "4d98362b6ab5c7ee88cf16fbf2ea0b0a16d99325dfe34faeba7dddcc54faaf74",
      "cleanup": "7e5ad676582b99099e0a19de746ace8b68b5312ff30c4600809da90378e73384"
    },
    "cp1252-every-byte-to-cp1251": {
      "metadata": "46240a0e434fe68446de2ad8ec5e4fc0ead9de062e3fb4fe9daec17752ed4355",
      "setup": "cbb8a8a7a1d3ee6fcdee603b499df38b0e9c6bc3b1636d0a54f431ddd342aa74",
      "execution": "838071afba8bdaed8b770df5e65bd50489b66c8569d0a23aa8b5131b3473c5aa",
      "readback": "4d98362b6ab5c7ee88cf16fbf2ea0b0a16d99325dfe34faeba7dddcc54faaf74",
      "cleanup": "7e5ad676582b99099e0a19de746ace8b68b5312ff30c4600809da90378e73384"
    },
    "cp1252-mixed-to-cp1251": {
      "metadata": "46240a0e434fe68446de2ad8ec5e4fc0ead9de062e3fb4fe9daec17752ed4355",
      "setup": "cbb8a8a7a1d3ee6fcdee603b499df38b0e9c6bc3b1636d0a54f431ddd342aa74",
      "execution": "d151ced1e97b099d99ad53d0fcacaddd6d25d50e3d775496a726f6eadfd1ab7d",
      "readback": "4d98362b6ab5c7ee88cf16fbf2ea0b0a16d99325dfe34faeba7dddcc54faaf74",
      "cleanup": "7e5ad676582b99099e0a19de746ace8b68b5312ff30c4600809da90378e73384"
    },
    "cp1252-fragmented-to-cp1251": {
      "metadata": "46240a0e434fe68446de2ad8ec5e4fc0ead9de062e3fb4fe9daec17752ed4355",
      "setup": "cbb8a8a7a1d3ee6fcdee603b499df38b0e9c6bc3b1636d0a54f431ddd342aa74",
      "execution": "dd81c2fa3cc0285373d8b8c6969a206fbb8aba78cc9de98b42bf295d1d08d5b6",
      "readback": "4d98362b6ab5c7ee88cf16fbf2ea0b0a16d99325dfe34faeba7dddcc54faaf74",
      "cleanup": "7e5ad676582b99099e0a19de746ace8b68b5312ff30c4600809da90378e73384"
    },
    "cp1252-every-byte-to-utf8": {
      "metadata": "46240a0e434fe68446de2ad8ec5e4fc0ead9de062e3fb4fe9daec17752ed4355",
      "setup": "b39cafedb52ac8c416fbc313e590e3cbd65c7842757df75ab05bbc4a4d67efa2",
      "execution": "838071afba8bdaed8b770df5e65bd50489b66c8569d0a23aa8b5131b3473c5aa",
      "readback": "4d98362b6ab5c7ee88cf16fbf2ea0b0a16d99325dfe34faeba7dddcc54faaf74",
      "cleanup": "7e5ad676582b99099e0a19de746ace8b68b5312ff30c4600809da90378e73384"
    },
    "cp1252-mixed-to-utf8": {
      "metadata": "46240a0e434fe68446de2ad8ec5e4fc0ead9de062e3fb4fe9daec17752ed4355",
      "setup": "b39cafedb52ac8c416fbc313e590e3cbd65c7842757df75ab05bbc4a4d67efa2",
      "execution": "d151ced1e97b099d99ad53d0fcacaddd6d25d50e3d775496a726f6eadfd1ab7d",
      "readback": "4d98362b6ab5c7ee88cf16fbf2ea0b0a16d99325dfe34faeba7dddcc54faaf74",
      "cleanup": "7e5ad676582b99099e0a19de746ace8b68b5312ff30c4600809da90378e73384"
    },
    "cp1252-fragmented-to-utf8": {
      "metadata": "46240a0e434fe68446de2ad8ec5e4fc0ead9de062e3fb4fe9daec17752ed4355",
      "setup": "b39cafedb52ac8c416fbc313e590e3cbd65c7842757df75ab05bbc4a4d67efa2",
      "execution": "dd81c2fa3cc0285373d8b8c6969a206fbb8aba78cc9de98b42bf295d1d08d5b6",
      "readback": "4d98362b6ab5c7ee88cf16fbf2ea0b0a16d99325dfe34faeba7dddcc54faaf74",
      "cleanup": "7e5ad676582b99099e0a19de746ace8b68b5312ff30c4600809da90378e73384"
    },
    "cp1252-every-byte-to-unicode": {
      "metadata": "46240a0e434fe68446de2ad8ec5e4fc0ead9de062e3fb4fe9daec17752ed4355",
      "setup": "69fcc49472b5b252febae436e5b84dc25c1e7e92398032fdabcf345baf9cd1a5",
      "execution": "838071afba8bdaed8b770df5e65bd50489b66c8569d0a23aa8b5131b3473c5aa",
      "readback": "4d98362b6ab5c7ee88cf16fbf2ea0b0a16d99325dfe34faeba7dddcc54faaf74",
      "cleanup": "7e5ad676582b99099e0a19de746ace8b68b5312ff30c4600809da90378e73384"
    },
    "cp1252-mixed-to-unicode": {
      "metadata": "46240a0e434fe68446de2ad8ec5e4fc0ead9de062e3fb4fe9daec17752ed4355",
      "setup": "69fcc49472b5b252febae436e5b84dc25c1e7e92398032fdabcf345baf9cd1a5",
      "execution": "d151ced1e97b099d99ad53d0fcacaddd6d25d50e3d775496a726f6eadfd1ab7d",
      "readback": "4d98362b6ab5c7ee88cf16fbf2ea0b0a16d99325dfe34faeba7dddcc54faaf74",
      "cleanup": "7e5ad676582b99099e0a19de746ace8b68b5312ff30c4600809da90378e73384"
    },
    "cp1252-fragmented-to-unicode": {
      "metadata": "46240a0e434fe68446de2ad8ec5e4fc0ead9de062e3fb4fe9daec17752ed4355",
      "setup": "69fcc49472b5b252febae436e5b84dc25c1e7e92398032fdabcf345baf9cd1a5",
      "execution": "dd81c2fa3cc0285373d8b8c6969a206fbb8aba78cc9de98b42bf295d1d08d5b6",
      "readback": "4d98362b6ab5c7ee88cf16fbf2ea0b0a16d99325dfe34faeba7dddcc54faaf74",
      "cleanup": "7e5ad676582b99099e0a19de746ace8b68b5312ff30c4600809da90378e73384"
    }
  },
  "responses": {
    "cp1252-every-byte-to-cp1252": {
      "metadata": "102f588e6a99a3002efb8e7a760bcd5b8497a8072d294229879ef94273790706",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "3396e76991bb053d990faa77b369cbd5dad329793037a57f237167bdd7d4c13c",
      "readback": "a7972f7b624734aece19900c69eef54d0cee1eba66c64db70f097106e3e63e0c",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-mixed-to-cp1252": {
      "metadata": "102f588e6a99a3002efb8e7a760bcd5b8497a8072d294229879ef94273790706",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "ae94f5777fde7eef5bd3e9db33812aa094e492f9598e10267543a0776e2c1874",
      "readback": "3a915073c6629c6bd4639e6a1c1cd5f3dd09c0129f0f9352bbd2d8db20a65c69",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-fragmented-to-cp1252": {
      "metadata": "102f588e6a99a3002efb8e7a760bcd5b8497a8072d294229879ef94273790706",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "1b16e88562d059cd9f5685517d64a379c8304750eff91fe37268a3b7f62f2097",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-every-byte-to-cp1251": {
      "metadata": "102f588e6a99a3002efb8e7a760bcd5b8497a8072d294229879ef94273790706",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "3396e76991bb053d990faa77b369cbd5dad329793037a57f237167bdd7d4c13c",
      "readback": "97af4836d8ccfeb5b685a15cb92ce711e3ee01f768811fd3cedb59fa6464dcdd",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-mixed-to-cp1251": {
      "metadata": "102f588e6a99a3002efb8e7a760bcd5b8497a8072d294229879ef94273790706",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "ae94f5777fde7eef5bd3e9db33812aa094e492f9598e10267543a0776e2c1874",
      "readback": "d1444da09efd442b5a779b2825ab1fc75e8e1bf2a731e663f9b6d8944a93584b",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-fragmented-to-cp1251": {
      "metadata": "102f588e6a99a3002efb8e7a760bcd5b8497a8072d294229879ef94273790706",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "684ebad4886588c5d9531f0835a7b59c872eacdbea0525fbcd4365d7e7986976",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-every-byte-to-utf8": {
      "metadata": "102f588e6a99a3002efb8e7a760bcd5b8497a8072d294229879ef94273790706",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "3396e76991bb053d990faa77b369cbd5dad329793037a57f237167bdd7d4c13c",
      "readback": "912214f9516d49f7af3be092e1b218482aaf28b7e161a7e9e277ce182e7347f7",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-mixed-to-utf8": {
      "metadata": "102f588e6a99a3002efb8e7a760bcd5b8497a8072d294229879ef94273790706",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "ae94f5777fde7eef5bd3e9db33812aa094e492f9598e10267543a0776e2c1874",
      "readback": "13a1567331c50179adb73210860b974d2a1f01fee56fab8e21a04019812d7ec5",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-fragmented-to-utf8": {
      "metadata": "102f588e6a99a3002efb8e7a760bcd5b8497a8072d294229879ef94273790706",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "c34c6d2b6eafddba56ca38d09bcf8f3b7a4c1550a4280f18b35d1c1dfe65f098",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-every-byte-to-unicode": {
      "metadata": "102f588e6a99a3002efb8e7a760bcd5b8497a8072d294229879ef94273790706",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "3396e76991bb053d990faa77b369cbd5dad329793037a57f237167bdd7d4c13c",
      "readback": "1ed65a55c4fbe6d4ffc6e86d195b98e951241518ff9d1e7effcd9cd58b859eee",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-mixed-to-unicode": {
      "metadata": "102f588e6a99a3002efb8e7a760bcd5b8497a8072d294229879ef94273790706",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "ae94f5777fde7eef5bd3e9db33812aa094e492f9598e10267543a0776e2c1874",
      "readback": "81438e6b558484bc899938888b0d343f60d8c93216893d46b5d7b89954331d58",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-fragmented-to-unicode": {
      "metadata": "102f588e6a99a3002efb8e7a760bcd5b8497a8072d294229879ef94273790706",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "9a0075942b8c72620d3bb67dc0d62c0c0c0067e7621de0c86c2a5fd6e994d43a",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    }
  },
  "fragmentation": {
    "cp1252-every-byte-to-cp1252": {
      "metadata": "2ce5635f390844831670aef6bb3b263c2162f6587d2738ee0117a4bacf442ece",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "d3e9f56cdceaef2438c7b6ad8fb4e70521204e58ddcbca9b085cd377309a8e34",
      "readback": "0d3e4d2c626831a62edc530623cee1621229311677904ec705314db7ff57ca94",
      "cleanup": "674122c8954ff58c11829eb2a2788a16a84bbb0527df8a408b6fcb997f76cf9b"
    },
    "cp1252-mixed-to-cp1252": {
      "metadata": "2ce5635f390844831670aef6bb3b263c2162f6587d2738ee0117a4bacf442ece",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "7189896fd06b0bba960d42ce30506a82e69166c3834f67a081a24c23657453b5",
      "readback": "2941a8cd7286cf70553883edbfa3e66776ead675d8c68b49669c2760e4eba96b",
      "cleanup": "674122c8954ff58c11829eb2a2788a16a84bbb0527df8a408b6fcb997f76cf9b"
    },
    "cp1252-fragmented-to-cp1252": {
      "metadata": "2ce5635f390844831670aef6bb3b263c2162f6587d2738ee0117a4bacf442ece",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "b907563d55cb7445b368e7754e7ef12cfa7f7a482d3a0e89bd4b9242b4fd4316",
      "readback": "ebdeb007353ce64e4703fb7a6b56d3d73b75f5bbac212276df10af7056afa68a",
      "cleanup": "674122c8954ff58c11829eb2a2788a16a84bbb0527df8a408b6fcb997f76cf9b"
    },
    "cp1252-every-byte-to-cp1251": {
      "metadata": "2ce5635f390844831670aef6bb3b263c2162f6587d2738ee0117a4bacf442ece",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "d3e9f56cdceaef2438c7b6ad8fb4e70521204e58ddcbca9b085cd377309a8e34",
      "readback": "0d3e4d2c626831a62edc530623cee1621229311677904ec705314db7ff57ca94",
      "cleanup": "674122c8954ff58c11829eb2a2788a16a84bbb0527df8a408b6fcb997f76cf9b"
    },
    "cp1252-mixed-to-cp1251": {
      "metadata": "2ce5635f390844831670aef6bb3b263c2162f6587d2738ee0117a4bacf442ece",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "7189896fd06b0bba960d42ce30506a82e69166c3834f67a081a24c23657453b5",
      "readback": "2941a8cd7286cf70553883edbfa3e66776ead675d8c68b49669c2760e4eba96b",
      "cleanup": "674122c8954ff58c11829eb2a2788a16a84bbb0527df8a408b6fcb997f76cf9b"
    },
    "cp1252-fragmented-to-cp1251": {
      "metadata": "2ce5635f390844831670aef6bb3b263c2162f6587d2738ee0117a4bacf442ece",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "b907563d55cb7445b368e7754e7ef12cfa7f7a482d3a0e89bd4b9242b4fd4316",
      "readback": "ebdeb007353ce64e4703fb7a6b56d3d73b75f5bbac212276df10af7056afa68a",
      "cleanup": "674122c8954ff58c11829eb2a2788a16a84bbb0527df8a408b6fcb997f76cf9b"
    },
    "cp1252-every-byte-to-utf8": {
      "metadata": "2ce5635f390844831670aef6bb3b263c2162f6587d2738ee0117a4bacf442ece",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "d3e9f56cdceaef2438c7b6ad8fb4e70521204e58ddcbca9b085cd377309a8e34",
      "readback": "b37f7c1f53fc9ca38e9336ec5ecb6d203fd2cb2e5b1a3145af2fcabf654461e6",
      "cleanup": "674122c8954ff58c11829eb2a2788a16a84bbb0527df8a408b6fcb997f76cf9b"
    },
    "cp1252-mixed-to-utf8": {
      "metadata": "2ce5635f390844831670aef6bb3b263c2162f6587d2738ee0117a4bacf442ece",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "7189896fd06b0bba960d42ce30506a82e69166c3834f67a081a24c23657453b5",
      "readback": "a498d2af9ac9cfff303dc9ec2c00da66bc71438093d1a1a64d67085c64ca6634",
      "cleanup": "674122c8954ff58c11829eb2a2788a16a84bbb0527df8a408b6fcb997f76cf9b"
    },
    "cp1252-fragmented-to-utf8": {
      "metadata": "2ce5635f390844831670aef6bb3b263c2162f6587d2738ee0117a4bacf442ece",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "b907563d55cb7445b368e7754e7ef12cfa7f7a482d3a0e89bd4b9242b4fd4316",
      "readback": "ecfa20b25cea8c85c98e23ae7bb69d1df538757251fb274ff18de64e46c3c429",
      "cleanup": "674122c8954ff58c11829eb2a2788a16a84bbb0527df8a408b6fcb997f76cf9b"
    },
    "cp1252-every-byte-to-unicode": {
      "metadata": "2ce5635f390844831670aef6bb3b263c2162f6587d2738ee0117a4bacf442ece",
      "setup": "fbd5b3a34c9f5880de6f8fc1db1814392ca7d32add6b3ad6017aa0e0e73569b5",
      "execution": "d3e9f56cdceaef2438c7b6ad8fb4e70521204e58ddcbca9b085cd377309a8e34",
      "readback": "b5d3946c37e78134242c45466cd9728229fc9097cabdd164c250da4781b74e2f",
      "cleanup": "674122c8954ff58c11829eb2a2788a16a84bbb0527df8a408b6fcb997f76cf9b"
    },
    "cp1252-mixed-to-unicode": {
      "metadata": "2ce5635f390844831670aef6bb3b263c2162f6587d2738ee0117a4bacf442ece",
      "setup": "fbd5b3a34c9f5880de6f8fc1db1814392ca7d32add6b3ad6017aa0e0e73569b5",
      "execution": "7189896fd06b0bba960d42ce30506a82e69166c3834f67a081a24c23657453b5",
      "readback": "a8e27f42e10a3dd2ea0a818c5efc7306ac78df801666fe51b47df5af4a63f226",
      "cleanup": "674122c8954ff58c11829eb2a2788a16a84bbb0527df8a408b6fcb997f76cf9b"
    },
    "cp1252-fragmented-to-unicode": {
      "metadata": "2ce5635f390844831670aef6bb3b263c2162f6587d2738ee0117a4bacf442ece",
      "setup": "fbd5b3a34c9f5880de6f8fc1db1814392ca7d32add6b3ad6017aa0e0e73569b5",
      "execution": "b907563d55cb7445b368e7754e7ef12cfa7f7a482d3a0e89bd4b9242b4fd4316",
      "readback": "0b5463ce5344e6d6008ccb06170a2f02fa84516c7ee564e74d3489973000ab08",
      "cleanup": "674122c8954ff58c11829eb2a2788a16a84bbb0527df8a408b6fcb997f76cf9b"
    }
  }
}
const RETAINED_SHA256 = 'd9d8baa3ce4530f1af077feda8c80c9394b110557748b1fb84df0db9201189c2'
const REVIEWED_RECAPTURE_SOURCES = []
// Actual SQL conversion, not a platform code-page table, is the authority.
const everyByte = Array.from({length:256}, (_,i) => i.toString(16).padStart(2,'0'))
const allBytes = everyByte.join('')
const targets = [['cp1252','varchar'],['cp1251','varchar'],['utf8','varchar'],['unicode','nvarchar']]
export const cases = []
for(const [target,targetFamily] of targets) for(const [kind,values] of [
  ['every-byte',[null,'',...everyByte]],
  ['mixed',[null,'','41818d908f9d42','00ff8081828d8f909d',allBytes]],
  ['fragmented',[null,'','41','41'.repeat(401)+allBytes.repeat(33)+'42'.repeat(402)+'818d908f9d']]
]) cases.push({declared:'cp1252',wire:'cp1252',target,sourceFamily:'varchar',sourceWidth:'max',wireWidth:'max',targetFamily,targetWidth:'max',name:`cp1252-${kind}-to-${target}`,values})
assert.equal(cases.length,12)
export function rowsFor(c) {
  assert.ok(c.values.length <= 260)
  for (const value of c.values) assert.ok(value === null || typeof value === 'string' && /^(?:[0-9a-f]{2})*$/.test(value) && value.length <= 65536)
  return c.values.map((valueHex,index) => ({id:index+1,valueHex}))
}
function failure(error) {return {name:error.name??'Error',code:error.code??null,message:String(error.message).slice(0,1024)}}
function requireCapture(record) {
  assert.ok(!record.result.traceFailure && !record.result.captureBudgetFailure, 'controlled capture interrupted; partial exchange retained')
}
async function sql(connection, text, retained) {
  return {sql: text, ...await exchange(connection, () => capture(connection, text), retained)}
}
function runBulk(connection, c, collation, rows) {
  return new Promise(resolveResult => {
    const errors = [], info = []
    let settled = false
    const finish = result => {if (settled) return; settled = true; cleanup(); resolveResult(result)}
    const fields = e => ({number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber,
      message: e.message, serverName: e.serverName, procName: e.procName})
    const onError = e => errors.push(fields(e)), onInfo = e => info.push(fields(e))
    const cleanup = () => {connection.off('errorMessage', onError); connection.off('infoMessage', onInfo); clearTimeout(timer)}
    const timer = setTimeout(() => {finish({errors, info, rowCount: null, error: {number: null, code: 'CAPTURE_TIMEOUT', message: 'Controlled bulk callback exceeded 15000ms'}}); connection.close()}, 15000)
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    try {
      const load = connection.newBulkLoad('dbo.cp1252_probe', {keepNulls: true}, (error, rowCount) => {
        finish({errors, info, rowCount: rowCount ?? null, error: error ? {number: error.number ?? null, code: error.code ?? null, message: error.message} : null})
      })
      load.addColumn('id', TYPES.Int, {nullable: false})
      const type = c.sourceFamily === 'char' ? TYPES.Char : TYPES.VarChar
      const raw = {...type, validate: value => {assert.ok(value === null || Buffer.isBuffer(value)); return value}}
      if (c.wire === 'omitted') raw.generateTypeInfo = (parameter, options) => type.generateTypeInfo(parameter, options).subarray(0, 3)
      load.addColumn('value', raw, {length: c.wireWidth === 'max' ? Infinity : c.wireWidth, nullable: true})
      load.columns[1].collation = collation
      load.getBulkInsertSql = () => `INSERT BULK dbo.cp1252_probe ([id] int, [value] ${c.sourceFamily}(${c.sourceWidth}) COLLATE ${profiles[c.declared]}) WITH (KEEP_NULLS)`
      connection.execBulkLoad(load, rows.map(row => ({id: row.id, value: row.valueHex === null ? null : Buffer.from(row.valueHex, 'hex')})))
    } catch (error) {finish({errors, info, rowCount: null, error: {number: error.number ?? null, code: error.code ?? null, message: error.message}})}
  })
}
const readbackSql = `SELECT @@ERROR AS last_error, @@ROWCOUNT AS last_rowcount, XACT_STATE() AS transaction_state, @@TRANCOUNT AS transaction_count;
SELECT id, value, CAST(value AS varbinary(max)) AS native_bytes, CAST(value AS nvarchar(max)) AS unicode_value,
CAST(CAST(value AS nvarchar(max)) AS varbinary(max)) AS unicode_units FROM dbo.cp1252_probe ORDER BY id;`
async function observe(config, initial, run, retained) {
  run.version = await sql(initial, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)) AS version, DB_NAME() AS database_name, CAST(SERVERPROPERTY('ServerName') AS nvarchar(128)) AS server_name", retained)
  requireCapture(run.version)
  assert.equal(run.version.result.errors.length, 0)
  const database = run.version.result.sets[0].rows[0][1]
  const caseConfig = {...config, options: {...config.options, database, packetSize: 512, requestTimeout: 10000}}
  run.observations = []
  for (const c of cases) {
    const o = {case: c, input: rowsFor(c)}
    retained.take(jsonSize(o, LIMIT))
    run.observations.push(o)
    let connection = await connect(caseConfig)
    try {
      const wireProfile = profiles[c.wire] ?? profiles.cp1252
      o.metadata = await sql(connection, `SELECT CAST('' AS ${c.sourceFamily}(${c.wireWidth})) COLLATE ${wireProfile} AS sample`, retained)
      requireCapture(o.metadata)
      assert.equal(o.metadata.result.errors.length, 0)
      const descriptor = o.metadata.result.sets[0].columns[0].collation
      const collation = new Collation(descriptor.lcid, descriptor.flags, descriptor.version, descriptor.sortId)
      if (c.wire === 'zero') collation.toBuffer = () => Buffer.alloc(5)
      o.wireCollationHex = c.wire === 'omitted' ? null : collation.toBuffer().toString('hex')
      const targetProfile = profiles[c.target] ?? profiles.cp1252
      const targetFamily = c.target === 'unicode' && c.targetFamily === 'varchar' ? 'nvarchar' : c.targetFamily
      o.setup = await sql(connection, `CREATE TABLE dbo.cp1252_probe (id int NOT NULL, value ${targetFamily}(${c.targetWidth}) COLLATE ${targetProfile} NULL)`, retained)
      requireCapture(o.setup)
      assert.equal(o.setup.result.errors.length, 0)
      o.execution = await exchange(connection, () => runBulk(connection, c, collation, o.input), retained)
      requireCapture(o.execution)
      o.readback = {session: 'original', ...await sql(connection, readbackSql, retained)}
      requireCapture(o.readback)
      if (connection.closed || o.readback.result.transportError || o.readback.result.errors?.some(e => e.number === null)) {
        connection.close()
        connection = await connect(caseConfig)
        // A replacement connection cannot establish the failed session's counters.
        o.recoveryReadback = {session: 'replacement', ...await sql(connection, readbackSql, retained)}
        requireCapture(o.recoveryReadback)
      }
      o.cleanup = await sql(connection, 'DROP TABLE dbo.cp1252_probe', retained)
      requireCapture(o.cleanup)
      assert.equal(o.cleanup.result.errors.length, 0)
      console.log(JSON.stringify({case: c.name, errors: o.execution.result.errors?.map(e => e.number), callback: o.execution.result.error?.code ?? null, count: o.execution.result.rowCount}))
    } finally {connection.close()}
  }
}
export function validateRequests(o) {
  const c = o.case
  const targetFamily = c.target === 'unicode' && c.targetFamily === 'varchar' ? 'nvarchar' : c.targetFamily
  assert.equal(o.metadata.sql, `SELECT CAST('' AS ${c.sourceFamily}(${c.wireWidth})) COLLATE ${profiles[c.wire] ?? profiles.cp1252} AS sample`, 'fixed metadata SQL')
  assert.equal(o.setup.sql, `CREATE TABLE dbo.cp1252_probe (id int NOT NULL, value ${targetFamily}(${c.targetWidth}) COLLATE ${profiles[c.target] ?? profiles.cp1252} NULL)`, 'fixed setup SQL')
  assert.equal(o.readback.sql, readbackSql, 'fixed readback SQL')
  if (o.recoveryReadback) assert.equal(o.recoveryReadback.sql, readbackSql, 'fixed recovery SQL')
  assert.equal(o.cleanup.sql, 'DROP TABLE dbo.cp1252_probe', 'fixed cleanup SQL')
  const expectedCollation = {cp1251: '1904002200', cp1252: '0904d00034', utf8: '0904002600', zero: '0000000000', omitted: null}[c.wire]
  assert.equal(o.wireCollationHex, expectedCollation, 'fixed supplied wire descriptor')
  const width = Buffer.alloc(2); width.writeUInt16LE(c.wireWidth === 'max' ? 65535 : c.wireWidth)
  const metadata = '81020000000000040026040269006400000000000500'
    + (c.sourceFamily === 'char' ? 'af' : 'a7') + width.toString('hex')
    + (expectedCollation ?? '') + '05760061006c0075006500'
  const rows = rowsFor(c).map(row => {
    const id = Buffer.alloc(4); id.writeInt32LE(row.id)
    let payload
    if (c.wireWidth !== 'max') {
      const length = Buffer.alloc(2); length.writeUInt16LE(row.valueHex === null ? 65535 : row.valueHex.length / 2)
      payload = length.toString('hex') + (row.valueHex ?? '')
    } else if (row.valueHex === null) payload = 'ffffffffffffffff'
    else {
      const length = Buffer.alloc(4); length.writeUInt32LE(row.valueHex.length / 2)
      payload = 'feffffffffffffff' + (row.valueHex.length ? length.toString('hex') + row.valueHex : '') + '00000000'
    }
    return 'd104' + id.toString('hex') + payload
  }).join('')
  const actual = o.execution.packets.filter(p => p.direction === 'out' && p.rawHex.startsWith('07')).map(p => p.rawHex.slice(16)).join('')
  assert.ok(actual === metadata + rows + 'fd000000000000000000000000', 'fixed original BulkLoad TYPE_INFO/ROW/PLP/DONE bytes')
}
export function requestSignature(record) {
  return createHash('sha256').update(record.packets.filter(p => p.direction === 'out').map(p => p.rawHex.slice(16)).join('')).digest('hex')
}
export function fragmentationSignature(record) {
  const headers = record.packets.map(p => ({direction: p.direction, header: p.rawHex.slice(0,8) + p.rawHex.slice(12,16)}))
  return createHash('sha256').update(JSON.stringify(headers)).digest('hex')
}
function validatePackets(record, expectedSpid, bulk = false) {
  assert.ok(record.packets.length <= MAX_PACKETS)
  const expectedMessages = bulk ? [['out',1],['in',4],['out',7],['in',4]] : [['out',1],['in',4]]
  let size = 0, message = null, inboundSpid = expectedSpid, messages = 0
  for (const p of record.packets) {
    assert.ok(['in','out'].includes(p.direction) && /^(?:[0-9a-f]{2})+$/.test(p.rawHex) && p.rawHex.length <= 65534)
    size += p.rawHex.length / 2
    assert.ok(size <= LIMIT)
    const bytes = Buffer.from(p.rawHex, 'hex')
    assert.ok(bytes.length >= 8 && bytes.readUInt16BE(2) === bytes.length)
    assert.ok(p.direction === 'in' ? bytes[0] === 4 : [1,6,7].includes(bytes[0]))
    assert.ok(bytes[1] === 0 || bytes[1] === 1, 'fixed controlled packet status')
    const spid = bytes.readUInt16BE(4)
    if (p.direction === 'in') {
      if (inboundSpid === undefined) inboundSpid = spid
      assert.equal(spid, inboundSpid, 'consistent inbound connection SPID')
    } else assert.equal(spid, 0, 'fixed outbound SPID')
    assert.equal(bytes[7], 0)
    if (message) {assert.equal(message.direction, p.direction); assert.equal(message.type, bytes[0])}
    else {
      assert.deepEqual([p.direction,bytes[0]], expectedMessages[messages], 'fixed phase message direction/type')
      message = {direction: p.direction, type: bytes[0], packetId: 1}
    }
    assert.equal(bytes[6], message.packetId)
    message.packetId = (message.packetId + 1) & 255
    if (bytes[1] & 1) {message = null; messages++}
  }
  assert.equal(message, null, 'complete retained EOM')
  assert.equal(messages, expectedMessages.length, 'complete fixed phase messages')
  return inboundSpid
}
export function semantic(o, database, serverName) {
  // Independent semantic digest only; original full packets/fields/differences
  // remain untouched. Ephemeral container identity is checked, not substituted.
  const value = structuredClone({metadata: o.metadata.result, setup: o.setup.result, execution: o.execution.result,
    readback: o.readback.result, recoveryReadback: o.recoveryReadback?.result ?? null, cleanup: o.cleanup.result})
  for (const result of Object.values(value)) if (result) for (const e of [...(result.errors ?? []), ...(result.info ?? [])]) {
    if (Object.hasOwn(e, 'serverName')) {assert.equal(e.serverName, serverName); delete e.serverName}
    if (e.number === 2628) {assert.ok(e.message.startsWith("String or binary data would be truncated in table '" + database + ".dbo.cp1252_probe', column 'value'. Truncated value: '")); e.message = e.message.replace(database, '@DATABASE@')}
  }
  if (value.execution.error?.number === 2628) {assert.ok(value.execution.error.message.includes(database + '.dbo.cp1252_probe')); value.execution.error.message = value.execution.error.message.replace(database, '@DATABASE@')}
  return createHash('sha256').update(JSON.stringify(value)).digest('hex')
}
export function validate(actual) {
  jsonSize(actual)
  assert.equal(actual.format, 1)
  assert.equal(actual.containers.length, 2)
  assert.equal(actual.runs.length, 4)
  for (const c of actual.containers) assert.equal(c.image, referenceImage)
  assert.equal(actual.provenance.clientVersion, '20.0.0')
  assert.equal(actual.provenance.packetSize, 512)
  assert.equal(actual.provenance.helperSha, EXPECTED_HELPER_SHA, 'fixed reviewed helper provenance')
  assert.ok(isDeepStrictEqual(actual.provenance.helpers, EXPECTED_HELPERS), 'fixed complete helper provenance')
  assert.match(actual.provenance.sourceSha, /^[0-9a-f]{64}$/)
  assert.ok(GOLD, 'actual capture has not yet been independently pinned')
  assert.equal(actual.provenance.nodeVersion, GOLD.nodeVersion, 'fixed actual Node provenance')
  assert.ok([GOLD.sourceSha, ...REVIEWED_RECAPTURE_SOURCES, SOURCE_SHA].includes(actual.provenance.sourceSha), 'fixed source provenance')
  const identities = actual.runs.map(run => run.version.result.sets[0].rows[0])
  assert.equal(new Set(identities.map(row => row[1])).size, 4, 'four independent database identities')
  assert.equal(identities[0][2], identities[1][2], 'first container server identity')
  assert.equal(identities[2][2], identities[3][2], 'second container server identity')
  assert.notEqual(identities[0][2], identities[2][2], 'two independent container identities')
  for (const run of actual.runs) {
    assert.equal(run.version.sql, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)) AS version, DB_NAME() AS database_name, CAST(SERVERPROPERTY('ServerName') AS nvarchar(128)) AS server_name", 'fixed version SQL')
    assert.equal(run.version.result.sets[0].rows[0][0], '17.0.4065.4')
    assert.equal(run.version.result.sets[0].rows[0].length, 3, 'fixed version identity row width')
    const [version, database, serverName] = run.version.result.sets[0].rows[0]
    assert.match(database, /^msduck_audit_[0-9a-f]{32}$/)
    assert.match(serverName, /^[0-9a-f]{12}$/)
    const versionResult = structuredClone(run.version.result)
    versionResult.sets[0].rows[0] = [version, '@DATABASE@', '@SERVER@']
    assert.equal(createHash('sha256').update(JSON.stringify(versionResult)).digest('hex'), GOLD.version, 'fixed complete version result')
    validatePackets(run.version)
    assert.equal(requestSignature(run.version), GOLD.versionRequest, 'fixed version request payload')
    assert.equal(responseSignature(run.version, database, serverName), GOLD.versionResponse, 'fixed version response payload')
    assert.equal(run.observations.length, cases.length)
    for (const [i, o] of run.observations.entries()) {
      assert.ok(isDeepStrictEqual(o.case, cases[i]), 'fixed probe specification')
      assert.ok(isDeepStrictEqual(o.input, rowsFor(cases[i])), 'fixed supplied bytes')
      assert.equal(o.readback.session, 'original', 'fixed readback session')
      if (o.recoveryReadback) assert.equal(o.recoveryReadback.session, 'replacement', 'fixed recovery session')
      validateRequests(o)
      assert.equal(semantic(o, database, serverName), GOLD.semantics[o.case.name], 'fixed independent semantic digest: ' + o.case.name)
      const originalSpid = validatePackets(o.metadata)
      const replacementSpid = o.recoveryReadback ? validatePackets(o.recoveryReadback) : originalSpid
      for (const phase of ['metadata','setup','execution','readback','recoveryReadback','cleanup']) if (o[phase]) {
        validatePackets(o[phase], phase === 'recoveryReadback' || phase === 'cleanup' ? replacementSpid : originalSpid, phase === 'execution')
        assert.equal(fragmentationSignature(o[phase]), GOLD.fragmentation[o.case.name][phase], 'fixed complete packet framing: ' + o.case.name + '/' + phase)
        assert.equal(requestSignature(o[phase]), GOLD.requests[o.case.name][phase], 'fixed request payload: ' + o.case.name + '/' + phase)
        assert.equal(responseSignature(o[phase], database, serverName), GOLD.responses[o.case.name][phase], 'fixed response payload: ' + o.case.name + '/' + phase)
      }
    }
  }
  assert.ok(isDeepStrictEqual(actual.comparisons, actual.runs.slice(1).map(run => compare(run, actual.runs[0]))), 'exact unnormalized comparisons')
}
export function validateRetained(bytes) {
  assert.ok(bytes.length <= CAPTURE_LIMIT && RETAINED_SHA256, 'bounded pinned retained artifact')
  assert.equal(createHash('sha256').update(bytes).digest('hex'), RETAINED_SHA256, 'fixed retained bytes')
  const value = JSON.parse(bytes.toString()); validate(value); return value
}
async function saveCapture(actual, output, captureError) {
  await guardOutput(output); await guardOutput(`${output}.comparison.json`)
  await mkdir(dirname(output), {recursive: true})
  const size = jsonSize(actual)
  const bytes = JSON.stringify(actual) + (size < CAPTURE_LIMIT ? '\n' : '')
  await writeFile(output, bytes, {flag: 'wx'})
  let expected, retainedBytes
  try {retainedBytes = await readCaptureFile(fixture); expected = JSON.parse(retainedBytes.toString())} catch (error) {if (error.code !== 'ENOENT') throw error}
  let comparison, comparisonError
  try {
    comparison = {retained: Boolean(expected), differences: expected ? compare(actual, expected) : [],
      ...(captureError ? {captureFailure: failure(captureError)} : {})}
    jsonSize(comparison)
  } catch (error) {
    comparisonError = error
    comparison = {retained: Boolean(expected), differencesOmitted: true, failure: failure(error),
      ...(captureError ? {captureFailure: failure(captureError)} : {})}
    jsonSize(comparison)
  }
  const comparisonSize = jsonSize(comparison)
  await writeFile(`${output}.comparison.json`, JSON.stringify(comparison) + (comparisonSize < CAPTURE_LIMIT ? '\n' : ''), {flag: 'wx'})
  if (comparisonError) throw comparisonError
  if (retainedBytes) validateRetained(retainedBytes)
  return {cases: cases.length, runs: actual.runs.length, output, sha256: createHash('sha256').update(bytes).digest('hex')}
}
export async function persistCapture(actual, output) {
  const result = await saveCapture(actual, output)
  validate(actual)
  return result
}
export async function finalizeCapture(raw, output) {
  jsonSize(raw)
  let actual
  try {
    actual = {...raw, comparisons: raw.runs.slice(1).map(run => compare(run, raw.runs[0]))}
    jsonSize(actual)
  } catch (error) {
    // Cross-run differences can multiply retained values. Preserve the bounded
    // original runs even when their derived comparisons exceed the envelope.
    // Failure metadata lives in the sidecar: adding it to near-limit raw data
    // must not make the original evidence exceed its previously checked bound.
    await saveCapture(raw, output, error)
    throw error
  }
  return persistCapture(actual, output)
}
export async function main(args = process.argv.slice(2)) {
  assert.ok(args.filter(a => a.startsWith('--')).every(a => a === '--replay-fixture'), 'known flags')
  const positional = args.filter(a => !a.startsWith('--'))
  assert.ok(positional.length <= 1)
  if (args.includes('--replay-fixture')) {validateRetained(await readCaptureFile(fixture)); console.log('Validated retained native CP1252 capture'); return}
  const output = resolve(positional[0] ?? 'artifacts/compatibility/bulk-character-cp1252/capture.json')
  assert.notEqual(output, fixture, 'separate raw output')
  await guardOutput(output); await guardOutput(`${output}.comparison.json`)
  const containers = [], runs = [], retained = budget()
  assert.equal(HELPER_SHA, EXPECTED_HELPER_SHA, 'reviewed helper unchanged before Docker')
  assert.ok(isDeepStrictEqual(HELPERS, EXPECTED_HELPERS), 'reviewed helpers unchanged before Docker')
  const provenance = {clientVersion: CLIENT_VERSION, packetSize: 512, nodeVersion: process.versions.node, sourceSha: SOURCE_SHA, helperSha: HELPER_SHA, helpers: HELPERS}
  try {
    for (let i = 0; i < 2; i++) await withReferenceContainer(async (config, container) => {
      containers.push({image: container.image})
      for (let db = 0; db < 2; db++) {
        const run = {}; runs.push(run)
        await isolatedReference(config, initial => observe(config, initial, run, retained))
      }
    }, {image:referenceImage})
  } catch (error) {
    const partial = {format: 1, provenance, containers, runs, failure: failure(error)}
    await saveCapture(partial, output)
    throw error
  }
  console.log(JSON.stringify(await finalizeCapture({format: 1, provenance, containers, runs}, output)))
}
if (process.argv[1] && resolve(process.argv[1]) === SOURCE) await main()
