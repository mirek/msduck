#!/usr/bin/env node
// Capacity-specific probe construction; bounded exchange/retention primitives
// reuse the owner-reviewed task895 capture, immutable at the pinned helper SHA.
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
const EXPECTED_HELPERS = {"capture-bulk-character-conversion.mjs":"9dd773a8170ea2c255d51bf40574fa75474563902c9211a2bceae7f5382e8868","lib/compatibility.mjs":"62f032fce72cf9a1fa0cb14bb2c7f1b5a2c396fdf4e8fcd5486d8db96aa0cf4d","lib/reference.mjs":"419a96611c0251e66b1f0783c9413151b4dacb5ee005ad89383402ff8c738df8","lib/reference-container.mjs":"d5b8d66c920330cbb4554cd16f1525f005eed9e3bdc69d5b00e421791545120c"}
const HELPERS = Object.fromEntries(await Promise.all(Object.keys(EXPECTED_HELPERS).map(async name => [name,createHash('sha256').update(await readFile(new URL(name,import.meta.url))).digest('hex')])))
const EXPECTED_HELPER_SHA = '9dd773a8170ea2c255d51bf40574fa75474563902c9211a2bceae7f5382e8868'
const fixture = fileURLToPath(new URL('../reference/bulk-character-capacity.json', import.meta.url))
const LIMIT = 2 * 1024 * 1024
const MAX_PACKETS = 1024
const GOLD = {
  "sourceSha": "0a62a38a082ab493620b5c26d074126f97cbf733cfa078fbcc725ded60dce227",
  "version": "1509703d2c117014f2615f5ff9fcdd4775a2e1a1db43be28520ccd71e9f919fd",
  "versionRequest": "3f611a97ed172bd33f68a477c73486bd09fb937d9d6cdd2e5783d763a434631f",
  "versionResponse": "fbb3249ff9242ae6fe494716c62f9ac3a6395dcfa7f152dfbbd1529e38dc3e68",
  "semantics": {
    "cp1251-varchar-to-cp1252-varchar-width1": "0d049e3464585c118171b856495af4b4748040e5e2c681cb016f839bd0961f06",
    "cp1251-varchar-to-cp1252-varchar-width2": "a6d2b5496fec6a18eaf3edc433dee8354e90ef11243a65d4a72620932ab647bb",
    "cp1251-varchar-to-cp1252-char-width1": "15ba17496adc6ad3ee7f0a081905cc98cddb6b2568c4f0bb9d80ba90e5576e72",
    "cp1251-varchar-to-cp1252-char-width2": "92c6f0b8a1fc5b809e84a089647b2e58fdebebc9d16cd3d87f01cd8c35a82f4a",
    "cp1251-varchar-to-utf8-varchar-width1": "cc050a7e0c23b47db7680f8428565310da30ddba68be60cd656a87c2dc538a0f",
    "cp1251-varchar-to-utf8-varchar-width2": "05bb4f438d4cfc53939cb8542e29bfa87b5c35f8c3b2c02b640a461f48b60c6c",
    "cp1251-varchar-to-utf8-char-width1": "3e50303c66dc053747927afb5609aad504bc8dee107291ad2f79a448561907b1",
    "cp1251-varchar-to-utf8-char-width2": "f2537b334d8a2d2a79a73e1613026dc8e08913d1e23c979563a7bdc07cbb020e",
    "cp1251-varchar-to-unicode-nvarchar-width1": "73098aa7798dbb3c41ff3b95fdb925ce438fc02e4ae8e0273526903e9526eb31",
    "cp1251-varchar-to-unicode-nvarchar-width2": "5ac25b9cef8dd8697ebd4f06fa506acf6ec948f4bb958cf805c98e4d3b7a5245",
    "cp1251-varchar-to-unicode-nchar-width1": "ef5ffce93dab932cda56dc71ab93c83dd5cd4437484ff0a8183ee4d25711beac",
    "cp1251-varchar-to-unicode-nchar-width2": "b8fb6e3434326dafd5a5277cdd18832f5be4a05cd58d48e94088092fe6ec1797",
    "cp1251-char-to-cp1252-varchar-width1": "0df64c9b99ed6a4606be3141d8b820722f6cf6221ded96becf30692ec051f5ac",
    "cp1251-char-to-cp1252-varchar-width2": "afb32bd267b0f7bb59f5b1b9c512fc3dcf155fbec8ba9c79dd9f356d89c9ce35",
    "cp1251-char-to-cp1252-char-width1": "3f86a171a417e2d3284dd90293a91a830b524d120184d19c50f1ad63862bf2ba",
    "cp1251-char-to-cp1252-char-width2": "3b9657b84948e47294d9b10aee332bc255ba44f5b6220bcca2147b18659ab59b",
    "cp1251-char-to-utf8-varchar-width1": "0e5c08d86d9713dcf5ec972c3a3506151431f7f0d7ac96d7fad3c797538a630b",
    "cp1251-char-to-utf8-varchar-width2": "5b4a41e8593ee437694658250060af34884829798ae9879cce96631a5b2019f3",
    "cp1251-char-to-utf8-char-width1": "3f2d2fc8a37e150e488be7e192b9b3783428c10b0a774607415ff6eeb3ea8375",
    "cp1251-char-to-utf8-char-width2": "53b0812afd92c876c2d6495091fb78b78d6373ad2eb18d60235cd9dc349c73fe",
    "cp1251-char-to-unicode-nvarchar-width1": "53158f56aeba1207471cfbe0bfdce7369ce375305e4741a668e663ba04ce943e",
    "cp1251-char-to-unicode-nvarchar-width2": "2cce71d5e0e79b78ce60bba967574af850b67f0ab897c5edeeb011878934e261",
    "cp1251-char-to-unicode-nchar-width1": "070b70f507ddcf41b11047d611a5322194bd0c4876a02a7808fb072fc275acd4",
    "cp1251-char-to-unicode-nchar-width2": "f9c0bcf660e3b196c353c4b379aaf25347add0004fd23ebd64bc4aef33bc5981",
    "cp1252-varchar-to-cp1252-varchar-width3": "9db4f270b02f159d93bf1f76f022ec600792bcb761d2e1779a0da4275be7816a",
    "cp1252-varchar-to-cp1252-varchar-width4": "a65864e7401838b02459c642beb02ae4754bea91db67218a0ec334142cbfb1b1",
    "cp1252-varchar-to-cp1252-char-width3": "c0ac807194aab24bb5f377ffbcb1923d8bcd9728446e18ccccadb2c5414f0ea0",
    "cp1252-varchar-to-cp1252-char-width4": "b93a7d431a894a9601b38cc9c5b1fd8c93793c2c1b779cc92250b52d3962881e",
    "cp1252-varchar-to-utf8-varchar-width3": "e701022e1542471c2aa92de18dec75422b72c3175964aa930fe166bdd0b626d9",
    "cp1252-varchar-to-utf8-varchar-width4": "e4ff6f7a60cb202339cbeff3c06820df185b1fcb783713375b8fcd50238882a9",
    "cp1252-varchar-to-utf8-char-width3": "2ba0657fe9a29e839fb8100706bdf7b3a14669453b36d524384c9d0cc2dde2fa",
    "cp1252-varchar-to-utf8-char-width4": "6ba5dc1e9b6dcae2ea7a427248365206c5b7c333a04f07359b51e74b7dd62330",
    "cp1252-varchar-to-unicode-nvarchar-width3": "e66cecc3269cb2c4b9b7281ddcc12a3ed4e4644e9b0866b76e1c20ce865da30c",
    "cp1251-row2-declared1-wire1": "ebc97325d381bd0dd0f875addc280f6e8653d0de02ad96e64fafe2e2ed8b2615",
    "cp1252-varchar-to-unicode-nchar-width3": "a1241302672b6437c1342401872285fcff56f37dd23e9c2421db0aa54196f324",
    "cp1252-varchar-to-unicode-nchar-width4": "b109f62342a90a127149cc59d8d81d1ffc084710fc08a8f1fbd31345910e361a",
    "cp1252-char-to-cp1252-varchar-width3": "cfe8278a3863d2ac315fb6d7ede58caf4d9036f16e5131b892ac506f7e7c218d",
    "cp1252-char-to-cp1252-varchar-width4": "78785ec5812c9e165f344d333eccb062f858e51a6655267afd10b122351ab0a9",
    "cp1252-char-to-cp1252-char-width3": "a4146cdff81a322ed7cb0a9fb88f87286af48add0ec86d3e7ca7fdd3fd51784e",
    "cp1252-char-to-cp1252-char-width4": "bb6fceb678dfb26e8d04ff81ec7d2725fd1a8445b3e6e4862fbd4a0f18812fa7",
    "cp1252-char-to-utf8-varchar-width3": "24d14c10a0b3aa44073a4a6066fe612a97efc3c63c154ed3d015105b04b60564",
    "cp1252-char-to-utf8-varchar-width4": "23013bb2ccab2d58ef863f47e6a17aae9f224d44d1be8763693559925cb8a393",
    "cp1252-char-to-utf8-char-width3": "fd240404cdaa7b7384f8d298a725be5e36911b76d3c2aca26f4bb8c4d159794d",
    "cp1252-char-to-utf8-char-width4": "e0f2ce57e6099a72b20cb9898b080db929f2269b641dcf74850c500f66bd3816",
    "cp1252-char-to-unicode-nvarchar-width3": "f22dc38d9c6404af134847f9a6960acdcc59e32baab7a81403c0f2b1d5a3291c",
    "cp1252-char-to-unicode-nvarchar-width4": "b4a974b581e749229d8473ac5a0a7b9a010ac50203ed57bc5f0cfc4b9e7a2ad3",
    "cp1252-char-to-unicode-nchar-width3": "5131a239495e1ebaa1a8fcb793afddead9ea02b1fd637f1a3d11216d39182c6e",
    "cp1252-char-to-unicode-nchar-width4": "8ad4634591f7b1497cb92c0c1ccdbdeb161429360385ae4f3fcb38a581cc8b6b",
    "utf8-varchar-to-cp1252-varchar-width1": "57cf6d5f6857de69af01c8170da4f141a2e931629ed96eb48f517f7cbb22f540",
    "utf8-varchar-to-cp1252-varchar-width2": "8a647db1d873288ecf888e989203e1e29079a0c03aa8d00a2fed857afcaf3203",
    "utf8-varchar-to-cp1252-char-width3": "7f713da7a50913657893dce0a4a8197ff6df93c13a85248d37fe189b3558cfa6",
    "utf8-varchar-to-cp1252-char-width4": "5e0b30c90e587a0c1f49719c2e717a991ed81e8b1d69d5d58869e3329bf0fce4",
    "utf8-varchar-to-utf8-varchar-width3": "ed68eb76a61a6b69253099b989d0c4e448706d8603307a79e646f96bf12eda0b",
    "utf8-varchar-to-utf8-varchar-width4": "b75f976af28ffe9169fd89180fcd952ac536c6c28a2ae60be874f78518dfff1a",
    "utf8-varchar-to-utf8-char-width2": "4d6006e584d37681c21e7a310f59a43c0f0b3dcc3767f40d00dd819883d0c3bb",
    "utf8-varchar-to-utf8-char-width3": "1b2a475ffad721c5c11c7d978c56cc0012bbf1aa9084575fad6918214b9c17c8",
    "utf8-varchar-to-unicode-nvarchar-width1": "4d7c8b2fba9a08216b0b8490047f527b88b041d29049d2fabf1c85048caf3774",
    "utf8-row2-declared1-wire1": "7724a34136c94906d2d2ab2004ac441df6b028846d2e30b57b3755e06bf1f6c5",
    "utf8-varchar-to-unicode-nchar-width1": "a3b9be694124a62468c2c75c6e89490ab082569bf9f48fd086a5b7195a128aac",
    "utf8-varchar-to-unicode-nchar-width2": "59e65838bbd42b6658ba8c4c88041d8ffe0cb751822db70ae4354ae45f5980c8",
    "utf8-char-to-cp1252-varchar-width1": "18c00cbec1fa0c9cc825bc42aad9235a8e842a794eda7f4c19b6f56e615497b9",
    "utf8-char-to-cp1252-varchar-width2": "f3c47d969946c2886d5abae4981c87582d18647f12544990d18fe5d12d53afbd",
    "utf8-char-to-cp1252-char-width3": "bbdb05d7ac6989490c2838a244646dd889de13595ae9f281ceabc915a2eeb4dc",
    "utf8-char-to-cp1252-char-width4": "2fc77207f25c217255aabca4ed58465dcd6d7455962b36cc678198e2a95994c2",
    "utf8-char-to-utf8-varchar-width3": "7e3af653a81901f7c1d62d89df4f1a3b97c3fa67419e55f724d25133328bcb05",
    "utf8-char-to-utf8-varchar-width4": "4c7254a2aaea5713b973c92c321d583be0b041930813417afb64fe6610863ea2",
    "utf8-char-to-utf8-char-width2": "f8797f7b5bc768ecaa9af5c75778588babbc106b826fafd8ab6fe2392ef03fd2",
    "utf8-char-to-utf8-char-width3": "f15770ba52f54ba9008bb042c809d8c0b533bc3dbb9a34cbb9f6971a0cd3ed54",
    "utf8-char-to-unicode-nvarchar-width1": "f1d3ab13e9a91dbdb1b2f66ed560487283dd932af67ea0cbd5e48b0124ea003a",
    "utf8-char-to-unicode-nvarchar-width2": "ae8c12bb8ce37d5d2b2cddda9c99f7bccb5ee704f6167bf3939bbc7cad5f138e",
    "utf8-char-to-unicode-nchar-width1": "7d56b8583413b3602fc2ef1672f607a67aaa307df301a132fcf2ef08998b84a2",
    "utf8-char-to-unicode-nchar-width2": "7fb18df34d6ccfe89c60272016a395435a5145d7891db069d31fcb393ee68779",
    "cp1251-varchar-declared1-wire2": "0f96dbb3bdb087b99a9fd253055152f15fdb60479eed5bbe2a6a654570bc9302",
    "cp1251-varchar-declared2-wire1": "8bd7d3db85c582085193b8c0407aa9f8e218a5cdb13b53a138cb2fef0755739d",
    "cp1251-char-declared1-wire2": "6ed22a70ea7d1b6c69755b0c5b2c402aecca61be664eaca0374b0a9f1282ad0a",
    "cp1251-char-declared2-wire1": "43911eb8a4049558b9eeb88dd411b215e7116c4f82a86f2d24c640cbb8aed4f0",
    "cp1252-varchar-declared1-wire2": "72310e96084414a963095ff258d5e7355d0949d5e45259889df8d4588268a588",
    "cp1252-varchar-declared2-wire1": "44fec512fc39b57f8245b57399e6680bb34a78a85ff779e7cdedd472a8e3dc08",
    "cp1252-char-declared1-wire2": "ac695a7f1646b95c6bed8be89c12c2c2f6891aec76626062a8c58f42de41f7d7",
    "cp1252-char-declared2-wire1": "b226afbb10a91b8a328c7ac7d655f5ae076eb613d10a778bdd7b78f1fd5c9fef",
    "utf8-varchar-declared1-wire2": "5d32d8fcc1496f83bf85e28bf1ea06bc53379dc56ebef747ef796118280d9923",
    "utf8-varchar-declared2-wire1": "e35d276d8f83836b60c757ae0f7b1edf3817565773c9ccd7d2a5f2683f210d99",
    "utf8-char-declared1-wire2": "a96058757b11567053a826f3ee3f23402597c42003c37fda414c5db1ab0de808",
    "utf8-char-declared2-wire1": "22b930e4f67231a461730ee388fb0da3a999ffb04d0412252cb067f937d6bec2",
    "cp1251-char-null-empty-to-char4": "82d476c7b107445ed59d6f33037c84a52f53667bab22c07a3db1a5b77f5f51bd",
    "cp1251-char-null-empty-to-nchar4": "be5848d043d77ccb95b0ea2d1bf019e867015f223ad5c6981a648b8ee16376c4",
    "cp1252-char-null-empty-to-char4": "9df38e19f9091af2f5110bfbe364c178681edc7cc76f864f44a57c9626178c30",
    "cp1252-char-null-empty-to-nchar4": "e6628931745061096382783092498f4cfb0909dac42a87331fc63b42778dffff",
    "utf8-char-null-empty-to-char4": "43df1f921f18800009b740847cad6bdd7b9fad51cc8292ae9eeab987e5460698",
    "utf8-char-null-empty-to-nchar4": "2346d57e4843be7909ae01c7985d72f61bb718854ecb0cb52a8b45fc809ca4ee",
    "cp1251-max-to-utf8-varchar-max": "b06c917cfad496626c6b82865f39a9d8aefaabd69733005bbf45d2efa41670ec",
    "cp1252-max-to-utf8-varchar-max": "49f4fd7baa1922f6238f170ae32cecd0f0c0bee2db4fe8b16b32700aae3e1b3b",
    "utf8-max-to-cp1252-varchar-max": "02fc7a14767d5fccb2a493e5963a0f69ad710992175e5e258e9a8a8b822b9400",
    "utf8-max-to-unicode-nvarchar-max": "0174ef47a8f624b3099378eec184b6964a4bef4e767758ef6a30720b1db128ab",
    "utf8-euro-to-cp1252-varchar-width1": "d8b52a7bf28d2bbaa9bf8d59d75dc2c95e9ff1e29e8f7da6123834dcaa4f940d",
    "utf8-euro-to-cp1252-varchar-width3": "ee9554beab0423d7fc599a4325d9c38f1510ebebb71f7d0f4c1f0b6f35f5ec54"
  },
  "requests": {
    "cp1251-varchar-to-cp1252-varchar-width1": {
      "metadata": "940665cf0f0242e73a99a427ad361cadd7f5592a85d167c3d7b595dff36538e8",
      "setup": "3fd4463c4fe5f91da2f90f005a78dc7d24f6d35e1b9466e333560a7e7fc06752",
      "execution": "1400714d8f0af4389ef11f41fd70dd2f8f616233ef160ba228626b1db5c8848c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-varchar-to-cp1252-varchar-width2": {
      "metadata": "940665cf0f0242e73a99a427ad361cadd7f5592a85d167c3d7b595dff36538e8",
      "setup": "b5d2006eee433d6958f48ef11da4b5323f7340f69ebf801438f37ced8ddca413",
      "execution": "1400714d8f0af4389ef11f41fd70dd2f8f616233ef160ba228626b1db5c8848c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-varchar-to-cp1252-char-width1": {
      "metadata": "940665cf0f0242e73a99a427ad361cadd7f5592a85d167c3d7b595dff36538e8",
      "setup": "c1282cb66ee175ad510f77db840bec7e9487362285b0d06d0e2276ee3ca98104",
      "execution": "1400714d8f0af4389ef11f41fd70dd2f8f616233ef160ba228626b1db5c8848c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-varchar-to-cp1252-char-width2": {
      "metadata": "940665cf0f0242e73a99a427ad361cadd7f5592a85d167c3d7b595dff36538e8",
      "setup": "a84e091878864336ec0a06c3d8442e65ee8e03ff0db4a285509979134bf1a610",
      "execution": "1400714d8f0af4389ef11f41fd70dd2f8f616233ef160ba228626b1db5c8848c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-varchar-to-utf8-varchar-width1": {
      "metadata": "940665cf0f0242e73a99a427ad361cadd7f5592a85d167c3d7b595dff36538e8",
      "setup": "e27e99be4bcdfd6f37129def55182df05dc34e0a8ee3c2bb114350333b3e5560",
      "execution": "1400714d8f0af4389ef11f41fd70dd2f8f616233ef160ba228626b1db5c8848c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-varchar-to-utf8-varchar-width2": {
      "metadata": "940665cf0f0242e73a99a427ad361cadd7f5592a85d167c3d7b595dff36538e8",
      "setup": "802bcd9653286f8d86de870da90870095e4f5f7d784b69a7b7283fc5c03b3cf9",
      "execution": "1400714d8f0af4389ef11f41fd70dd2f8f616233ef160ba228626b1db5c8848c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-varchar-to-utf8-char-width1": {
      "metadata": "a05e12e038747f944bc07d1df597e14bafb90bbaf39302ee7bc632b286ea3508",
      "setup": "a773bce5e3e78365a117981126a9d1326195bc13adc315748f150a964fdb7179",
      "execution": "41e5f2fbe2f4b5fdcf1ea25a0cf51f98fc570937ec14f5c673254a1ba419a270",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-varchar-to-utf8-char-width2": {
      "metadata": "a05e12e038747f944bc07d1df597e14bafb90bbaf39302ee7bc632b286ea3508",
      "setup": "95c6a9ad9ea4124b36b5a304a9c57bbb2376c4624c22033c8d380939773ba9c1",
      "execution": "41e5f2fbe2f4b5fdcf1ea25a0cf51f98fc570937ec14f5c673254a1ba419a270",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-varchar-to-unicode-nvarchar-width1": {
      "metadata": "940665cf0f0242e73a99a427ad361cadd7f5592a85d167c3d7b595dff36538e8",
      "setup": "cca802edb701492a36a17a173ad653dfd5d8b3954cbbb62cd25afbe1dffb806c",
      "execution": "1400714d8f0af4389ef11f41fd70dd2f8f616233ef160ba228626b1db5c8848c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-varchar-to-unicode-nvarchar-width2": {
      "metadata": "940665cf0f0242e73a99a427ad361cadd7f5592a85d167c3d7b595dff36538e8",
      "setup": "f33d856fc6e601c6fc69f181ff1634ce8025796192a567b480f4a53d75e1bf86",
      "execution": "1400714d8f0af4389ef11f41fd70dd2f8f616233ef160ba228626b1db5c8848c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-varchar-to-unicode-nchar-width1": {
      "metadata": "940665cf0f0242e73a99a427ad361cadd7f5592a85d167c3d7b595dff36538e8",
      "setup": "b10c345700350b2f4fa1a1d6dcd06815d455d8b7d177839dafdd9ee93139c7ba",
      "execution": "1400714d8f0af4389ef11f41fd70dd2f8f616233ef160ba228626b1db5c8848c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-varchar-to-unicode-nchar-width2": {
      "metadata": "940665cf0f0242e73a99a427ad361cadd7f5592a85d167c3d7b595dff36538e8",
      "setup": "b46af7ae25d3585b3d34a22aaa75695f8b85ac196dce4502cd81362999f75d82",
      "execution": "1400714d8f0af4389ef11f41fd70dd2f8f616233ef160ba228626b1db5c8848c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-char-to-cp1252-varchar-width1": {
      "metadata": "10e3352e3d00145d8e107751e24e878dbb6ab8e9d7916f75ba84dfaf0dd9947d",
      "setup": "3fd4463c4fe5f91da2f90f005a78dc7d24f6d35e1b9466e333560a7e7fc06752",
      "execution": "c04614d35a59fc71b1259305587c51351fba3634785081eba552702b97c1fc4c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-char-to-cp1252-varchar-width2": {
      "metadata": "10e3352e3d00145d8e107751e24e878dbb6ab8e9d7916f75ba84dfaf0dd9947d",
      "setup": "b5d2006eee433d6958f48ef11da4b5323f7340f69ebf801438f37ced8ddca413",
      "execution": "c04614d35a59fc71b1259305587c51351fba3634785081eba552702b97c1fc4c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-char-to-cp1252-char-width1": {
      "metadata": "10e3352e3d00145d8e107751e24e878dbb6ab8e9d7916f75ba84dfaf0dd9947d",
      "setup": "c1282cb66ee175ad510f77db840bec7e9487362285b0d06d0e2276ee3ca98104",
      "execution": "c04614d35a59fc71b1259305587c51351fba3634785081eba552702b97c1fc4c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-char-to-cp1252-char-width2": {
      "metadata": "10e3352e3d00145d8e107751e24e878dbb6ab8e9d7916f75ba84dfaf0dd9947d",
      "setup": "a84e091878864336ec0a06c3d8442e65ee8e03ff0db4a285509979134bf1a610",
      "execution": "c04614d35a59fc71b1259305587c51351fba3634785081eba552702b97c1fc4c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-char-to-utf8-varchar-width1": {
      "metadata": "10e3352e3d00145d8e107751e24e878dbb6ab8e9d7916f75ba84dfaf0dd9947d",
      "setup": "e27e99be4bcdfd6f37129def55182df05dc34e0a8ee3c2bb114350333b3e5560",
      "execution": "c04614d35a59fc71b1259305587c51351fba3634785081eba552702b97c1fc4c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-char-to-utf8-varchar-width2": {
      "metadata": "10e3352e3d00145d8e107751e24e878dbb6ab8e9d7916f75ba84dfaf0dd9947d",
      "setup": "802bcd9653286f8d86de870da90870095e4f5f7d784b69a7b7283fc5c03b3cf9",
      "execution": "c04614d35a59fc71b1259305587c51351fba3634785081eba552702b97c1fc4c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-char-to-utf8-char-width1": {
      "metadata": "358147bcac1e148dd189cc6e90d266284ba56e1c2f44092efbf32d94a16e61c3",
      "setup": "a773bce5e3e78365a117981126a9d1326195bc13adc315748f150a964fdb7179",
      "execution": "a9874dc07415e5a72071a03e1b8d73d6381e4e08737c8837da43e7a758fe1ed3",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-char-to-utf8-char-width2": {
      "metadata": "358147bcac1e148dd189cc6e90d266284ba56e1c2f44092efbf32d94a16e61c3",
      "setup": "95c6a9ad9ea4124b36b5a304a9c57bbb2376c4624c22033c8d380939773ba9c1",
      "execution": "a9874dc07415e5a72071a03e1b8d73d6381e4e08737c8837da43e7a758fe1ed3",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-char-to-unicode-nvarchar-width1": {
      "metadata": "10e3352e3d00145d8e107751e24e878dbb6ab8e9d7916f75ba84dfaf0dd9947d",
      "setup": "cca802edb701492a36a17a173ad653dfd5d8b3954cbbb62cd25afbe1dffb806c",
      "execution": "c04614d35a59fc71b1259305587c51351fba3634785081eba552702b97c1fc4c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-char-to-unicode-nvarchar-width2": {
      "metadata": "10e3352e3d00145d8e107751e24e878dbb6ab8e9d7916f75ba84dfaf0dd9947d",
      "setup": "f33d856fc6e601c6fc69f181ff1634ce8025796192a567b480f4a53d75e1bf86",
      "execution": "c04614d35a59fc71b1259305587c51351fba3634785081eba552702b97c1fc4c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-char-to-unicode-nchar-width1": {
      "metadata": "10e3352e3d00145d8e107751e24e878dbb6ab8e9d7916f75ba84dfaf0dd9947d",
      "setup": "b10c345700350b2f4fa1a1d6dcd06815d455d8b7d177839dafdd9ee93139c7ba",
      "execution": "c04614d35a59fc71b1259305587c51351fba3634785081eba552702b97c1fc4c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-char-to-unicode-nchar-width2": {
      "metadata": "10e3352e3d00145d8e107751e24e878dbb6ab8e9d7916f75ba84dfaf0dd9947d",
      "setup": "b46af7ae25d3585b3d34a22aaa75695f8b85ac196dce4502cd81362999f75d82",
      "execution": "c04614d35a59fc71b1259305587c51351fba3634785081eba552702b97c1fc4c",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-varchar-to-cp1252-varchar-width3": {
      "metadata": "49bf34071e33a80960e64dc860341a1876f8a10ed24e407c508994a4da2a44d0",
      "setup": "415d3efff05b607c05a63e3652a0037b5ec2373ec35b60b84fde5ad62c698fc5",
      "execution": "3ab1ea2faebe4f12120981f063b8cf2c155db6690d2730dcdec50ac84bfe8c80",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-varchar-to-cp1252-varchar-width4": {
      "metadata": "49bf34071e33a80960e64dc860341a1876f8a10ed24e407c508994a4da2a44d0",
      "setup": "b8cacaea3f1572184bc8fe6b2dd38c95ba707d39c5cc2bdf00f35436be72b442",
      "execution": "3ab1ea2faebe4f12120981f063b8cf2c155db6690d2730dcdec50ac84bfe8c80",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-varchar-to-cp1252-char-width3": {
      "metadata": "49bf34071e33a80960e64dc860341a1876f8a10ed24e407c508994a4da2a44d0",
      "setup": "8c8c0bc08cf11f2435517e00867656f7a02b9f86e630e5fc91d539534de9a412",
      "execution": "3ab1ea2faebe4f12120981f063b8cf2c155db6690d2730dcdec50ac84bfe8c80",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-varchar-to-cp1252-char-width4": {
      "metadata": "49bf34071e33a80960e64dc860341a1876f8a10ed24e407c508994a4da2a44d0",
      "setup": "c0dbfbfdbf8e1daca9842f2a2bb81d26497c9fcf4ce1890060644daedcbeac8a",
      "execution": "3ab1ea2faebe4f12120981f063b8cf2c155db6690d2730dcdec50ac84bfe8c80",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-varchar-to-utf8-varchar-width3": {
      "metadata": "49bf34071e33a80960e64dc860341a1876f8a10ed24e407c508994a4da2a44d0",
      "setup": "78a39cf85611f7191b8f58f196efa04131022a83b691ed114b28d260361cdeb0",
      "execution": "3ab1ea2faebe4f12120981f063b8cf2c155db6690d2730dcdec50ac84bfe8c80",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-varchar-to-utf8-varchar-width4": {
      "metadata": "49bf34071e33a80960e64dc860341a1876f8a10ed24e407c508994a4da2a44d0",
      "setup": "23d1f1481f79eaca4395cc1d0147b4b8f916b955547d2f6526f6f987e53d5b76",
      "execution": "3ab1ea2faebe4f12120981f063b8cf2c155db6690d2730dcdec50ac84bfe8c80",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-varchar-to-utf8-char-width3": {
      "metadata": "d5230a33fc1aa443c7d7912cc7389cb9fcff4f2f44d53ad7aa4db31d8e2faf90",
      "setup": "52b4a78c4df372dbd28894b4c1b2b764f6d8505864038eee653e6a67ff35f7c9",
      "execution": "4b11bf79ce0f8fd4dcb891cc817bc302b16847cc114f5ccaa20a031e23a785a6",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-varchar-to-utf8-char-width4": {
      "metadata": "d5230a33fc1aa443c7d7912cc7389cb9fcff4f2f44d53ad7aa4db31d8e2faf90",
      "setup": "fdcca1c2254fb3022fd629422058b9e634b91b9ceb548f045233f434d1806bb9",
      "execution": "4b11bf79ce0f8fd4dcb891cc817bc302b16847cc114f5ccaa20a031e23a785a6",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-varchar-to-unicode-nvarchar-width3": {
      "metadata": "49bf34071e33a80960e64dc860341a1876f8a10ed24e407c508994a4da2a44d0",
      "setup": "993625ffa742e835ec22093c2cff89f0b5e4e273591317ecee966eee9abe5944",
      "execution": "3ab1ea2faebe4f12120981f063b8cf2c155db6690d2730dcdec50ac84bfe8c80",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-row2-declared1-wire1": {
      "metadata": "aa392fdd46eddfa49fa4397f75996ea5a3629e787b64e58a89aaa98264a26ab3",
      "setup": "fdcca1c2254fb3022fd629422058b9e634b91b9ceb548f045233f434d1806bb9",
      "execution": "55080551daffc01780abc4f100b88dacfe32f9034afcf650b3332a7d8b1ceb94",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-varchar-to-unicode-nchar-width3": {
      "metadata": "49bf34071e33a80960e64dc860341a1876f8a10ed24e407c508994a4da2a44d0",
      "setup": "b573032a4642711b01ef9b68979d077a3e5d28cb375541a6e14dfcd6a229b99a",
      "execution": "3ab1ea2faebe4f12120981f063b8cf2c155db6690d2730dcdec50ac84bfe8c80",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-varchar-to-unicode-nchar-width4": {
      "metadata": "49bf34071e33a80960e64dc860341a1876f8a10ed24e407c508994a4da2a44d0",
      "setup": "42b76d377dea4d817958e6911ace33a3147ce166cfa644bd3f8cf095fec6aacc",
      "execution": "3ab1ea2faebe4f12120981f063b8cf2c155db6690d2730dcdec50ac84bfe8c80",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-char-to-cp1252-varchar-width3": {
      "metadata": "d034bffcc36be13a1295a8ddd9992445e8c8d0e61a3a2d2cbff456b24dddec9d",
      "setup": "415d3efff05b607c05a63e3652a0037b5ec2373ec35b60b84fde5ad62c698fc5",
      "execution": "9715dd163a5ee977fd09a867ccd59233a975fa01702f5882467bcd589fe11a6f",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-char-to-cp1252-varchar-width4": {
      "metadata": "d034bffcc36be13a1295a8ddd9992445e8c8d0e61a3a2d2cbff456b24dddec9d",
      "setup": "b8cacaea3f1572184bc8fe6b2dd38c95ba707d39c5cc2bdf00f35436be72b442",
      "execution": "9715dd163a5ee977fd09a867ccd59233a975fa01702f5882467bcd589fe11a6f",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-char-to-cp1252-char-width3": {
      "metadata": "d034bffcc36be13a1295a8ddd9992445e8c8d0e61a3a2d2cbff456b24dddec9d",
      "setup": "8c8c0bc08cf11f2435517e00867656f7a02b9f86e630e5fc91d539534de9a412",
      "execution": "9715dd163a5ee977fd09a867ccd59233a975fa01702f5882467bcd589fe11a6f",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-char-to-cp1252-char-width4": {
      "metadata": "d034bffcc36be13a1295a8ddd9992445e8c8d0e61a3a2d2cbff456b24dddec9d",
      "setup": "c0dbfbfdbf8e1daca9842f2a2bb81d26497c9fcf4ce1890060644daedcbeac8a",
      "execution": "9715dd163a5ee977fd09a867ccd59233a975fa01702f5882467bcd589fe11a6f",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-char-to-utf8-varchar-width3": {
      "metadata": "d034bffcc36be13a1295a8ddd9992445e8c8d0e61a3a2d2cbff456b24dddec9d",
      "setup": "78a39cf85611f7191b8f58f196efa04131022a83b691ed114b28d260361cdeb0",
      "execution": "9715dd163a5ee977fd09a867ccd59233a975fa01702f5882467bcd589fe11a6f",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-char-to-utf8-varchar-width4": {
      "metadata": "d034bffcc36be13a1295a8ddd9992445e8c8d0e61a3a2d2cbff456b24dddec9d",
      "setup": "23d1f1481f79eaca4395cc1d0147b4b8f916b955547d2f6526f6f987e53d5b76",
      "execution": "9715dd163a5ee977fd09a867ccd59233a975fa01702f5882467bcd589fe11a6f",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-char-to-utf8-char-width3": {
      "metadata": "e13791d8a07a69405349aaec05a653433cca7242a2ce5c3c0db7cdb79d09b3e2",
      "setup": "52b4a78c4df372dbd28894b4c1b2b764f6d8505864038eee653e6a67ff35f7c9",
      "execution": "e363ca50cec478fa1f016bd6b7729a390b759716c3e8a8b501df442da65eba2a",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-char-to-utf8-char-width4": {
      "metadata": "e13791d8a07a69405349aaec05a653433cca7242a2ce5c3c0db7cdb79d09b3e2",
      "setup": "fdcca1c2254fb3022fd629422058b9e634b91b9ceb548f045233f434d1806bb9",
      "execution": "e363ca50cec478fa1f016bd6b7729a390b759716c3e8a8b501df442da65eba2a",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-char-to-unicode-nvarchar-width3": {
      "metadata": "d034bffcc36be13a1295a8ddd9992445e8c8d0e61a3a2d2cbff456b24dddec9d",
      "setup": "993625ffa742e835ec22093c2cff89f0b5e4e273591317ecee966eee9abe5944",
      "execution": "9715dd163a5ee977fd09a867ccd59233a975fa01702f5882467bcd589fe11a6f",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-char-to-unicode-nvarchar-width4": {
      "metadata": "d034bffcc36be13a1295a8ddd9992445e8c8d0e61a3a2d2cbff456b24dddec9d",
      "setup": "2f60be4fb6594fe7378ebd3492d94a7f583a3765968c0e01a2c1295bccfad51b",
      "execution": "9715dd163a5ee977fd09a867ccd59233a975fa01702f5882467bcd589fe11a6f",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-char-to-unicode-nchar-width3": {
      "metadata": "d034bffcc36be13a1295a8ddd9992445e8c8d0e61a3a2d2cbff456b24dddec9d",
      "setup": "b573032a4642711b01ef9b68979d077a3e5d28cb375541a6e14dfcd6a229b99a",
      "execution": "9715dd163a5ee977fd09a867ccd59233a975fa01702f5882467bcd589fe11a6f",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-char-to-unicode-nchar-width4": {
      "metadata": "d034bffcc36be13a1295a8ddd9992445e8c8d0e61a3a2d2cbff456b24dddec9d",
      "setup": "42b76d377dea4d817958e6911ace33a3147ce166cfa644bd3f8cf095fec6aacc",
      "execution": "9715dd163a5ee977fd09a867ccd59233a975fa01702f5882467bcd589fe11a6f",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-varchar-to-cp1252-varchar-width1": {
      "metadata": "82b36bfb0b469ffa1794e335f1e4d960feb7298253c78d0c748c33626aeb8f0d",
      "setup": "3fd4463c4fe5f91da2f90f005a78dc7d24f6d35e1b9466e333560a7e7fc06752",
      "execution": "002e669733454fab4a3f0d77a3ed22aaa5bfed6a751aa73f050fdc011de10a3d",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-varchar-to-cp1252-varchar-width2": {
      "metadata": "82b36bfb0b469ffa1794e335f1e4d960feb7298253c78d0c748c33626aeb8f0d",
      "setup": "b5d2006eee433d6958f48ef11da4b5323f7340f69ebf801438f37ced8ddca413",
      "execution": "002e669733454fab4a3f0d77a3ed22aaa5bfed6a751aa73f050fdc011de10a3d",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-varchar-to-cp1252-char-width3": {
      "metadata": "cda4b2c93c09c4c6f41e2bdb517f5ed327cc579f67513ae6c215b6db8582cfd5",
      "setup": "8c8c0bc08cf11f2435517e00867656f7a02b9f86e630e5fc91d539534de9a412",
      "execution": "913d9a8728693b20f78fc7a2c9498474ffcd30d2c58f3a8ce84ddf4a479d503d",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-varchar-to-cp1252-char-width4": {
      "metadata": "cda4b2c93c09c4c6f41e2bdb517f5ed327cc579f67513ae6c215b6db8582cfd5",
      "setup": "c0dbfbfdbf8e1daca9842f2a2bb81d26497c9fcf4ce1890060644daedcbeac8a",
      "execution": "913d9a8728693b20f78fc7a2c9498474ffcd30d2c58f3a8ce84ddf4a479d503d",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-varchar-to-utf8-varchar-width3": {
      "metadata": "cda4b2c93c09c4c6f41e2bdb517f5ed327cc579f67513ae6c215b6db8582cfd5",
      "setup": "78a39cf85611f7191b8f58f196efa04131022a83b691ed114b28d260361cdeb0",
      "execution": "913d9a8728693b20f78fc7a2c9498474ffcd30d2c58f3a8ce84ddf4a479d503d",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-varchar-to-utf8-varchar-width4": {
      "metadata": "cda4b2c93c09c4c6f41e2bdb517f5ed327cc579f67513ae6c215b6db8582cfd5",
      "setup": "23d1f1481f79eaca4395cc1d0147b4b8f916b955547d2f6526f6f987e53d5b76",
      "execution": "913d9a8728693b20f78fc7a2c9498474ffcd30d2c58f3a8ce84ddf4a479d503d",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-varchar-to-utf8-char-width2": {
      "metadata": "a41da61cd392d852a11cbd074dbfb796f26824b0cbfaf90aa8e5b2ea02dd5538",
      "setup": "95c6a9ad9ea4124b36b5a304a9c57bbb2376c4624c22033c8d380939773ba9c1",
      "execution": "9d2f2cd72742ea97ae949f23562ed13183240d3a52034462a5017af566f9c761",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-varchar-to-utf8-char-width3": {
      "metadata": "a41da61cd392d852a11cbd074dbfb796f26824b0cbfaf90aa8e5b2ea02dd5538",
      "setup": "52b4a78c4df372dbd28894b4c1b2b764f6d8505864038eee653e6a67ff35f7c9",
      "execution": "9d2f2cd72742ea97ae949f23562ed13183240d3a52034462a5017af566f9c761",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-varchar-to-unicode-nvarchar-width1": {
      "metadata": "a41da61cd392d852a11cbd074dbfb796f26824b0cbfaf90aa8e5b2ea02dd5538",
      "setup": "cca802edb701492a36a17a173ad653dfd5d8b3954cbbb62cd25afbe1dffb806c",
      "execution": "9d2f2cd72742ea97ae949f23562ed13183240d3a52034462a5017af566f9c761",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-row2-declared1-wire1": {
      "metadata": "388268a64029135da71c0a81fe9e5da195b2b971cf70cf77b2b63b3f0c251821",
      "setup": "fdcca1c2254fb3022fd629422058b9e634b91b9ceb548f045233f434d1806bb9",
      "execution": "dceb32ecaa6330ed5e7452231e19a384523d086b2f70789f0ce3b39e1fc5b3e9",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-varchar-to-unicode-nchar-width1": {
      "metadata": "cda4b2c93c09c4c6f41e2bdb517f5ed327cc579f67513ae6c215b6db8582cfd5",
      "setup": "b10c345700350b2f4fa1a1d6dcd06815d455d8b7d177839dafdd9ee93139c7ba",
      "execution": "913d9a8728693b20f78fc7a2c9498474ffcd30d2c58f3a8ce84ddf4a479d503d",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-varchar-to-unicode-nchar-width2": {
      "metadata": "cda4b2c93c09c4c6f41e2bdb517f5ed327cc579f67513ae6c215b6db8582cfd5",
      "setup": "b46af7ae25d3585b3d34a22aaa75695f8b85ac196dce4502cd81362999f75d82",
      "execution": "913d9a8728693b20f78fc7a2c9498474ffcd30d2c58f3a8ce84ddf4a479d503d",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-char-to-cp1252-varchar-width1": {
      "metadata": "bcf936622e6787b5ed7e006e0cb943e0011e214334b5047d9bb68ae3cf450170",
      "setup": "3fd4463c4fe5f91da2f90f005a78dc7d24f6d35e1b9466e333560a7e7fc06752",
      "execution": "8fac91d07210214e23760432ab45ef0fc9ba936ff0ad8728ab865a49a4e74094",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-char-to-cp1252-varchar-width2": {
      "metadata": "bcf936622e6787b5ed7e006e0cb943e0011e214334b5047d9bb68ae3cf450170",
      "setup": "b5d2006eee433d6958f48ef11da4b5323f7340f69ebf801438f37ced8ddca413",
      "execution": "8fac91d07210214e23760432ab45ef0fc9ba936ff0ad8728ab865a49a4e74094",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-char-to-cp1252-char-width3": {
      "metadata": "825b6b695ef55de6c22490d273eb085d3f50556744523572f6932504d6d7aa99",
      "setup": "8c8c0bc08cf11f2435517e00867656f7a02b9f86e630e5fc91d539534de9a412",
      "execution": "b0a1e25430c77bc4caac7645db6c1c07a5ac252b0bc2f8e44461126b576d2fbe",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-char-to-cp1252-char-width4": {
      "metadata": "825b6b695ef55de6c22490d273eb085d3f50556744523572f6932504d6d7aa99",
      "setup": "c0dbfbfdbf8e1daca9842f2a2bb81d26497c9fcf4ce1890060644daedcbeac8a",
      "execution": "b0a1e25430c77bc4caac7645db6c1c07a5ac252b0bc2f8e44461126b576d2fbe",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-char-to-utf8-varchar-width3": {
      "metadata": "825b6b695ef55de6c22490d273eb085d3f50556744523572f6932504d6d7aa99",
      "setup": "78a39cf85611f7191b8f58f196efa04131022a83b691ed114b28d260361cdeb0",
      "execution": "b0a1e25430c77bc4caac7645db6c1c07a5ac252b0bc2f8e44461126b576d2fbe",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-char-to-utf8-varchar-width4": {
      "metadata": "825b6b695ef55de6c22490d273eb085d3f50556744523572f6932504d6d7aa99",
      "setup": "23d1f1481f79eaca4395cc1d0147b4b8f916b955547d2f6526f6f987e53d5b76",
      "execution": "b0a1e25430c77bc4caac7645db6c1c07a5ac252b0bc2f8e44461126b576d2fbe",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-char-to-utf8-char-width2": {
      "metadata": "db13a905bc0e883b3fa169532f11443fa5f68909335543bbc582ab6dcf911efe",
      "setup": "95c6a9ad9ea4124b36b5a304a9c57bbb2376c4624c22033c8d380939773ba9c1",
      "execution": "765058e0ef891af223a925f57cd301e5bcd6f875b3dd726c8fe98327e030bffa",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-char-to-utf8-char-width3": {
      "metadata": "db13a905bc0e883b3fa169532f11443fa5f68909335543bbc582ab6dcf911efe",
      "setup": "52b4a78c4df372dbd28894b4c1b2b764f6d8505864038eee653e6a67ff35f7c9",
      "execution": "765058e0ef891af223a925f57cd301e5bcd6f875b3dd726c8fe98327e030bffa",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-char-to-unicode-nvarchar-width1": {
      "metadata": "db13a905bc0e883b3fa169532f11443fa5f68909335543bbc582ab6dcf911efe",
      "setup": "cca802edb701492a36a17a173ad653dfd5d8b3954cbbb62cd25afbe1dffb806c",
      "execution": "765058e0ef891af223a925f57cd301e5bcd6f875b3dd726c8fe98327e030bffa",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-char-to-unicode-nvarchar-width2": {
      "metadata": "db13a905bc0e883b3fa169532f11443fa5f68909335543bbc582ab6dcf911efe",
      "setup": "f33d856fc6e601c6fc69f181ff1634ce8025796192a567b480f4a53d75e1bf86",
      "execution": "765058e0ef891af223a925f57cd301e5bcd6f875b3dd726c8fe98327e030bffa",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-char-to-unicode-nchar-width1": {
      "metadata": "825b6b695ef55de6c22490d273eb085d3f50556744523572f6932504d6d7aa99",
      "setup": "b10c345700350b2f4fa1a1d6dcd06815d455d8b7d177839dafdd9ee93139c7ba",
      "execution": "b0a1e25430c77bc4caac7645db6c1c07a5ac252b0bc2f8e44461126b576d2fbe",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-char-to-unicode-nchar-width2": {
      "metadata": "825b6b695ef55de6c22490d273eb085d3f50556744523572f6932504d6d7aa99",
      "setup": "b46af7ae25d3585b3d34a22aaa75695f8b85ac196dce4502cd81362999f75d82",
      "execution": "b0a1e25430c77bc4caac7645db6c1c07a5ac252b0bc2f8e44461126b576d2fbe",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-varchar-declared1-wire2": {
      "metadata": "940665cf0f0242e73a99a427ad361cadd7f5592a85d167c3d7b595dff36538e8",
      "setup": "7f4efe69e754ee1f9161d156077431d6e33d71cba0203b717dd13846ae2ca613",
      "execution": "a1e8ec2c00302fbf2dfe596258e3e525d5848e5d84e48338f9790a712aa8b454",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-varchar-declared2-wire1": {
      "metadata": "e09a00e54f292017bc63cf09fcbbab758a398342016a9a49f1ea9b87bf633272",
      "setup": "7f4efe69e754ee1f9161d156077431d6e33d71cba0203b717dd13846ae2ca613",
      "execution": "f32b0cad0e4af881dd61b618de9b60d5d0e5d0d940c45810523baa0b591e9d53",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-char-declared1-wire2": {
      "metadata": "10e3352e3d00145d8e107751e24e878dbb6ab8e9d7916f75ba84dfaf0dd9947d",
      "setup": "7f4efe69e754ee1f9161d156077431d6e33d71cba0203b717dd13846ae2ca613",
      "execution": "613c4737f13d3f740ef7c93a610c8cd36b96df8e1f61b4d9276a311b9cf171c4",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-char-declared2-wire1": {
      "metadata": "aa392fdd46eddfa49fa4397f75996ea5a3629e787b64e58a89aaa98264a26ab3",
      "setup": "7f4efe69e754ee1f9161d156077431d6e33d71cba0203b717dd13846ae2ca613",
      "execution": "7a67b66700f81dd37c72a496f324d49d3356a6d8f775e60bbcb1ff40c4358353",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-varchar-declared1-wire2": {
      "metadata": "49bf34071e33a80960e64dc860341a1876f8a10ed24e407c508994a4da2a44d0",
      "setup": "7f4efe69e754ee1f9161d156077431d6e33d71cba0203b717dd13846ae2ca613",
      "execution": "e92e1800e963e78e79ab4b12c526e79e9bfdb8e5c50a20d69b258227bb319e85",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-varchar-declared2-wire1": {
      "metadata": "bf2c87d278de2af4cdd204cc27aab5115aea807f3ffac07a58bc6af78e51ba0d",
      "setup": "7f4efe69e754ee1f9161d156077431d6e33d71cba0203b717dd13846ae2ca613",
      "execution": "8554ebc7760479293be6b0a235fc032d0a8d2c41c29f0ec179b85817b378069d",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-char-declared1-wire2": {
      "metadata": "d034bffcc36be13a1295a8ddd9992445e8c8d0e61a3a2d2cbff456b24dddec9d",
      "setup": "7f4efe69e754ee1f9161d156077431d6e33d71cba0203b717dd13846ae2ca613",
      "execution": "97d9bd9353074d388244db3c3a505d106f32c9467bf2587dbacfd11b93ada902",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-char-declared2-wire1": {
      "metadata": "93956db17a37fec04af43c0811b7e78424ac57b0d0955957ee0ea780f60324c2",
      "setup": "7f4efe69e754ee1f9161d156077431d6e33d71cba0203b717dd13846ae2ca613",
      "execution": "521feda96c11811a1404a8e3dc3521b6e5e40a1ac5dc7ebe6a766f543fce52b8",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-varchar-declared1-wire2": {
      "metadata": "82b36bfb0b469ffa1794e335f1e4d960feb7298253c78d0c748c33626aeb8f0d",
      "setup": "7f4efe69e754ee1f9161d156077431d6e33d71cba0203b717dd13846ae2ca613",
      "execution": "f9fc95e4aff87a9c2f2e56d38813bc5680c7629fff128693eea5a5749280b5d2",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-varchar-declared2-wire1": {
      "metadata": "7e53631dc4f611e28f6034c28c94a9b4bfe63862a70f8b3b4f847b1be7fd66a1",
      "setup": "7f4efe69e754ee1f9161d156077431d6e33d71cba0203b717dd13846ae2ca613",
      "execution": "8e21f036d83abe3d80212c74d242a155df76c796dc4e8bb02cafe49674bedeb4",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-char-declared1-wire2": {
      "metadata": "bcf936622e6787b5ed7e006e0cb943e0011e214334b5047d9bb68ae3cf450170",
      "setup": "7f4efe69e754ee1f9161d156077431d6e33d71cba0203b717dd13846ae2ca613",
      "execution": "41d3a921f763c3163b4d48b106dac577d5e2d5215061117bfe3b07c129233c7f",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-char-declared2-wire1": {
      "metadata": "388268a64029135da71c0a81fe9e5da195b2b971cf70cf77b2b63b3f0c251821",
      "setup": "7f4efe69e754ee1f9161d156077431d6e33d71cba0203b717dd13846ae2ca613",
      "execution": "144a245fd85cf2cffe1a27d90ef0a867ba03188a3f5f299e8815247f3cef37ee",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-char-null-empty-to-char4": {
      "metadata": "aa392fdd46eddfa49fa4397f75996ea5a3629e787b64e58a89aaa98264a26ab3",
      "setup": "fdcca1c2254fb3022fd629422058b9e634b91b9ceb548f045233f434d1806bb9",
      "execution": "fc003062970b09a717509f24a46f66f772489fa58ec49c80b8f451223a23bb00",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-char-null-empty-to-nchar4": {
      "metadata": "aa392fdd46eddfa49fa4397f75996ea5a3629e787b64e58a89aaa98264a26ab3",
      "setup": "42b76d377dea4d817958e6911ace33a3147ce166cfa644bd3f8cf095fec6aacc",
      "execution": "fc003062970b09a717509f24a46f66f772489fa58ec49c80b8f451223a23bb00",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-char-null-empty-to-char4": {
      "metadata": "93956db17a37fec04af43c0811b7e78424ac57b0d0955957ee0ea780f60324c2",
      "setup": "fdcca1c2254fb3022fd629422058b9e634b91b9ceb548f045233f434d1806bb9",
      "execution": "1b9ee4e0fc147657a554c2da71d026213ea4f03ab54341757c7e022372d517ef",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-char-null-empty-to-nchar4": {
      "metadata": "93956db17a37fec04af43c0811b7e78424ac57b0d0955957ee0ea780f60324c2",
      "setup": "42b76d377dea4d817958e6911ace33a3147ce166cfa644bd3f8cf095fec6aacc",
      "execution": "1b9ee4e0fc147657a554c2da71d026213ea4f03ab54341757c7e022372d517ef",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-char-null-empty-to-char4": {
      "metadata": "388268a64029135da71c0a81fe9e5da195b2b971cf70cf77b2b63b3f0c251821",
      "setup": "fdcca1c2254fb3022fd629422058b9e634b91b9ceb548f045233f434d1806bb9",
      "execution": "5f34ea7b04931ee05b51941b131a7e460a1d22e764d285746de887318b8613f6",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-char-null-empty-to-nchar4": {
      "metadata": "388268a64029135da71c0a81fe9e5da195b2b971cf70cf77b2b63b3f0c251821",
      "setup": "42b76d377dea4d817958e6911ace33a3147ce166cfa644bd3f8cf095fec6aacc",
      "execution": "5f34ea7b04931ee05b51941b131a7e460a1d22e764d285746de887318b8613f6",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1251-max-to-utf8-varchar-max": {
      "metadata": "169db311e2310e787b8b00e63bba1866b8f5ed7c5f2db37836fbdde8ed0369c6",
      "setup": "7f4efe69e754ee1f9161d156077431d6e33d71cba0203b717dd13846ae2ca613",
      "execution": "33d723f70e79860dd89914f7068763f11411795ed7c3a2266877f310dff88abd",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "cp1252-max-to-utf8-varchar-max": {
      "metadata": "46240a0e434fe68446de2ad8ec5e4fc0ead9de062e3fb4fe9daec17752ed4355",
      "setup": "7f4efe69e754ee1f9161d156077431d6e33d71cba0203b717dd13846ae2ca613",
      "execution": "6f1188592a27812e3f293ad9c3efe19154703ba1ea9d5ab57ab8f04078859e7f",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-max-to-cp1252-varchar-max": {
      "metadata": "9134eb5b8f42b613dc4d56cd50d70646003088975a1c78152ceac29b191a6b26",
      "setup": "a08a3125caf31994329792e465fe99c300e1c018aa30625a5bc00a0e1e7ef5fd",
      "execution": "17580d08e8633225fbeb6bcfeae5e6b38bf449a5093b034290f7fffd9ae2f0d7",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-max-to-unicode-nvarchar-max": {
      "metadata": "9134eb5b8f42b613dc4d56cd50d70646003088975a1c78152ceac29b191a6b26",
      "setup": "5ca6d63c38b540bb396f25da9136018b4e926c0d511500172626d26662d0e716",
      "execution": "17580d08e8633225fbeb6bcfeae5e6b38bf449a5093b034290f7fffd9ae2f0d7",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-euro-to-cp1252-varchar-width1": {
      "metadata": "a41da61cd392d852a11cbd074dbfb796f26824b0cbfaf90aa8e5b2ea02dd5538",
      "setup": "3fd4463c4fe5f91da2f90f005a78dc7d24f6d35e1b9466e333560a7e7fc06752",
      "execution": "9d2f2cd72742ea97ae949f23562ed13183240d3a52034462a5017af566f9c761",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    },
    "utf8-euro-to-cp1252-varchar-width3": {
      "metadata": "a41da61cd392d852a11cbd074dbfb796f26824b0cbfaf90aa8e5b2ea02dd5538",
      "setup": "415d3efff05b607c05a63e3652a0037b5ec2373ec35b60b84fde5ad62c698fc5",
      "execution": "9d2f2cd72742ea97ae949f23562ed13183240d3a52034462a5017af566f9c761",
      "readback": "5c612a952fea0baaccac6152392bc56788581cd7be1fa692beb55c0d38f4cb4d",
      "cleanup": "0ba3bae70da6b5eac124a592148d8a792a27d293f11f940677a175d8b527eb52"
    }
  },
  "responses": {
    "cp1251-varchar-to-cp1252-varchar-width1": {
      "metadata": "0c4fb211bfa69b909882ba384f001d35ee79cb0488205d92888e225c2fd1a4df",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "480588bb4375240cc7a6baa5b22426a97aa49944ff7069806e32ebaf04100f76",
      "readback": "85dbd7d3c02d099115d90e0eb3a2e02b9b2ad88f4baa503a5cc7d26175fd2284",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-varchar-to-cp1252-varchar-width2": {
      "metadata": "0c4fb211bfa69b909882ba384f001d35ee79cb0488205d92888e225c2fd1a4df",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "2ac1a32563add323e457ea34282eac7db409a1433a4605c23f471363ca8072ad",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-varchar-to-cp1252-char-width1": {
      "metadata": "0c4fb211bfa69b909882ba384f001d35ee79cb0488205d92888e225c2fd1a4df",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "480588bb4375240cc7a6baa5b22426a97aa49944ff7069806e32ebaf04100f76",
      "readback": "b345854336999ccddbfceaf13ff750abf605beb0236d0d6d2946a6a61126d7f3",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-varchar-to-cp1252-char-width2": {
      "metadata": "0c4fb211bfa69b909882ba384f001d35ee79cb0488205d92888e225c2fd1a4df",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "f02af7f77aa66169e7ff438623fdcb68412179fa03a1df7efd0afc87c98ae5ee",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-varchar-to-utf8-varchar-width1": {
      "metadata": "0c4fb211bfa69b909882ba384f001d35ee79cb0488205d92888e225c2fd1a4df",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4d6f8b51a0c776732ea6e10cb29e3da4ea6890969792df34852e011e269b64e6",
      "readback": "0efd85cdf392a55efd57eb6fa763b245257c7ecdf506417b840a18b2efaeadff",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-varchar-to-utf8-varchar-width2": {
      "metadata": "0c4fb211bfa69b909882ba384f001d35ee79cb0488205d92888e225c2fd1a4df",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "4e29765c5d6f735f8c090c98ea14215a03e7534b6879f89df10ef4a5b488f12d",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-varchar-to-utf8-char-width1": {
      "metadata": "2fc126f9681759ad9dda85978d0da1def83725a48b6395cfe434f5160c9e70fb",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "3fd7ad057e6edc1dfb52b344584440299beef4097948cc586cfca95b3d579f1c",
      "readback": "dbdb80e20f15a38d02b0d58f024bd871a6ca6bf2be22a640a3c1b0e8080e8c34",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-varchar-to-utf8-char-width2": {
      "metadata": "2fc126f9681759ad9dda85978d0da1def83725a48b6395cfe434f5160c9e70fb",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "0794cf672974d9f55d57c038fe280b59e883584b409c9c20e4c7046d3f35618b",
      "readback": "703de746602ce8cde00bc32b71b70dd69bfd747cb7fda4253c79ba57c0636098",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-varchar-to-unicode-nvarchar-width1": {
      "metadata": "0c4fb211bfa69b909882ba384f001d35ee79cb0488205d92888e225c2fd1a4df",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "55267e8366d517d88ee62ff70c706d49d14bdd04d2cfa8c41b1b5fa847973483",
      "readback": "ace887a2f9037536152412592bd5cf8fe7c0ceb51b1e121595b20ba2df7997c0",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-varchar-to-unicode-nvarchar-width2": {
      "metadata": "0c4fb211bfa69b909882ba384f001d35ee79cb0488205d92888e225c2fd1a4df",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "5554d0bb592c57682d4e59a74d45fb3d3f776d3a758717a2e42fae40febe45d2",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-varchar-to-unicode-nchar-width1": {
      "metadata": "0c4fb211bfa69b909882ba384f001d35ee79cb0488205d92888e225c2fd1a4df",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "55267e8366d517d88ee62ff70c706d49d14bdd04d2cfa8c41b1b5fa847973483",
      "readback": "cc6c27a196eab2b7d479ccc0efb6aa7f1072aaea469537ca23ea438b5c3386ab",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-varchar-to-unicode-nchar-width2": {
      "metadata": "0c4fb211bfa69b909882ba384f001d35ee79cb0488205d92888e225c2fd1a4df",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "4f36598337e03dc8d675974771da5721a78fe21b0d92029f416997964bb9d7eb",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-char-to-cp1252-varchar-width1": {
      "metadata": "0a8c1e8c07e6731e5c825712898e66879abe643e77f02d91bd11988c6c15eef1",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "480588bb4375240cc7a6baa5b22426a97aa49944ff7069806e32ebaf04100f76",
      "readback": "85dbd7d3c02d099115d90e0eb3a2e02b9b2ad88f4baa503a5cc7d26175fd2284",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-char-to-cp1252-varchar-width2": {
      "metadata": "0a8c1e8c07e6731e5c825712898e66879abe643e77f02d91bd11988c6c15eef1",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "96d913250af48338a935ae7768b5630c9c1bc5a8bafa7451fd7ef002cb495738",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-char-to-cp1252-char-width1": {
      "metadata": "0a8c1e8c07e6731e5c825712898e66879abe643e77f02d91bd11988c6c15eef1",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "480588bb4375240cc7a6baa5b22426a97aa49944ff7069806e32ebaf04100f76",
      "readback": "b345854336999ccddbfceaf13ff750abf605beb0236d0d6d2946a6a61126d7f3",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-char-to-cp1252-char-width2": {
      "metadata": "0a8c1e8c07e6731e5c825712898e66879abe643e77f02d91bd11988c6c15eef1",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "9a9d9b9e1c5d19ab3b7bc56c8aba9ceedda9e2e4952810cf781f189acb4b1c5f",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-char-to-utf8-varchar-width1": {
      "metadata": "0a8c1e8c07e6731e5c825712898e66879abe643e77f02d91bd11988c6c15eef1",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4d6f8b51a0c776732ea6e10cb29e3da4ea6890969792df34852e011e269b64e6",
      "readback": "0efd85cdf392a55efd57eb6fa763b245257c7ecdf506417b840a18b2efaeadff",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-char-to-utf8-varchar-width2": {
      "metadata": "0a8c1e8c07e6731e5c825712898e66879abe643e77f02d91bd11988c6c15eef1",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "7e4787d8f4646284572e295e3d11fabcaaa1697d35eba3e7bce3ed2892cd3eb0",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-char-to-utf8-char-width1": {
      "metadata": "f59733001b1412950ecfd898777e6e1c53950b1df91ce8f310ac926becab866f",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "3fd7ad057e6edc1dfb52b344584440299beef4097948cc586cfca95b3d579f1c",
      "readback": "dbdb80e20f15a38d02b0d58f024bd871a6ca6bf2be22a640a3c1b0e8080e8c34",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-char-to-utf8-char-width2": {
      "metadata": "f59733001b1412950ecfd898777e6e1c53950b1df91ce8f310ac926becab866f",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "0794cf672974d9f55d57c038fe280b59e883584b409c9c20e4c7046d3f35618b",
      "readback": "703de746602ce8cde00bc32b71b70dd69bfd747cb7fda4253c79ba57c0636098",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-char-to-unicode-nvarchar-width1": {
      "metadata": "0a8c1e8c07e6731e5c825712898e66879abe643e77f02d91bd11988c6c15eef1",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "55267e8366d517d88ee62ff70c706d49d14bdd04d2cfa8c41b1b5fa847973483",
      "readback": "ace887a2f9037536152412592bd5cf8fe7c0ceb51b1e121595b20ba2df7997c0",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-char-to-unicode-nvarchar-width2": {
      "metadata": "0a8c1e8c07e6731e5c825712898e66879abe643e77f02d91bd11988c6c15eef1",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "5c927439723e6dc3885ce7bfaab721f751cdf4100cb7affe79816fabb68bb31c",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-char-to-unicode-nchar-width1": {
      "metadata": "0a8c1e8c07e6731e5c825712898e66879abe643e77f02d91bd11988c6c15eef1",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "55267e8366d517d88ee62ff70c706d49d14bdd04d2cfa8c41b1b5fa847973483",
      "readback": "cc6c27a196eab2b7d479ccc0efb6aa7f1072aaea469537ca23ea438b5c3386ab",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-char-to-unicode-nchar-width2": {
      "metadata": "0a8c1e8c07e6731e5c825712898e66879abe643e77f02d91bd11988c6c15eef1",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "ffb8dca6f2c5292d0783ccb3bd4e4ac70e1412b41bff807a8142990c69b633bc",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-varchar-to-cp1252-varchar-width3": {
      "metadata": "701b639a40d068cb318638b522868b955527e3b368006e86e835a387b250410a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "9996f4644fb433acb04ac617e0a8598b0cd4d5ab8b9b689d1e2aa257149862ea",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-varchar-to-cp1252-varchar-width4": {
      "metadata": "701b639a40d068cb318638b522868b955527e3b368006e86e835a387b250410a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "f56b95314f105d216b4691e321ca5be4b3d9ea025d51b4bddf2db4a8ab29974f",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-varchar-to-cp1252-char-width3": {
      "metadata": "701b639a40d068cb318638b522868b955527e3b368006e86e835a387b250410a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "7496006dba8e243842c82150b6b9e7654f0617cede94d6c7b43de47dcb728e81",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-varchar-to-cp1252-char-width4": {
      "metadata": "701b639a40d068cb318638b522868b955527e3b368006e86e835a387b250410a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "794b47530a5c1e5c8080ad1231b162727a45f163820ddf7726a293fb48cf310f",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-varchar-to-utf8-varchar-width3": {
      "metadata": "701b639a40d068cb318638b522868b955527e3b368006e86e835a387b250410a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "5a16e26b8ce335f57cbc95090ccc56755fc5e142dbb8f19b7afe430ed6a180f6",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-varchar-to-utf8-varchar-width4": {
      "metadata": "701b639a40d068cb318638b522868b955527e3b368006e86e835a387b250410a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "5e795a0e1be1c9091563011b1857b8a5020ccce9224f64329ad290360eefe8ea",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-varchar-to-utf8-char-width3": {
      "metadata": "fcd92c9d1af09381c85a1dffad48614a4138768c6f52d8ce4c37977600e84735",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "60d96a81c74b29377f111cb82d9fd00189640d0245d3fe2b57bc8e642b3648cc",
      "readback": "12d0d3be860259268c2896f832f709d967f55a5eb9d8f41c12b48c4435d7a1dc",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-varchar-to-utf8-char-width4": {
      "metadata": "fcd92c9d1af09381c85a1dffad48614a4138768c6f52d8ce4c37977600e84735",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "88ff4c5d4e45b5718be18c2150abd391167f3fd17c334d54223b830e5851168c",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-varchar-to-unicode-nvarchar-width3": {
      "metadata": "701b639a40d068cb318638b522868b955527e3b368006e86e835a387b250410a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "03aaa3d95b59a6a678e46857dfcac4513e8055140264ba0d82441e7caf0c5080",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-row2-declared1-wire1": {
      "metadata": "2ca2a4237fd04c8f435a4e75c192ee6002f655dccc89af79b179a464aa3cccf7",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "ca542dfa117a13196067eeb099b8931e2342db67abb3ea9677fc53bc5e851308",
      "readback": "8cd12242dbff31d06b8a46817160b85c16277eb7ffac5357b3168a0717ca32a3",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-varchar-to-unicode-nchar-width3": {
      "metadata": "701b639a40d068cb318638b522868b955527e3b368006e86e835a387b250410a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "00e14b914b72352c522500b8c9dbf5fd31e84f0ffc604fc8373a0ce9cd5be18a",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-varchar-to-unicode-nchar-width4": {
      "metadata": "701b639a40d068cb318638b522868b955527e3b368006e86e835a387b250410a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "b6f7cbbebe2835e603f99cff266716be46c824229a39cdcb70f27017658ffe67",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-char-to-cp1252-varchar-width3": {
      "metadata": "d07af4e7778540ce7675d67742d26ead548652ebcb7b38830bf10447a722cf07",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "6d2b1e67a240b07a81e86b2ec641c58ecfd40112ece37453e18983b81f851095",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-char-to-cp1252-varchar-width4": {
      "metadata": "d07af4e7778540ce7675d67742d26ead548652ebcb7b38830bf10447a722cf07",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "a0bd6eb1f0cd9fd474c36390db278b21c32891c47cd0a1dc018db53502003fbb",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-char-to-cp1252-char-width3": {
      "metadata": "d07af4e7778540ce7675d67742d26ead548652ebcb7b38830bf10447a722cf07",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "d4791bfe721f29c7a838454b65591d58326e3ef904236ac93690fe13227a7781",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-char-to-cp1252-char-width4": {
      "metadata": "d07af4e7778540ce7675d67742d26ead548652ebcb7b38830bf10447a722cf07",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "c7b4529b285116e8e2d61bd2f5fc949d85f6b8f5fb7b140b84fdc13da7bc5014",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-char-to-utf8-varchar-width3": {
      "metadata": "d07af4e7778540ce7675d67742d26ead548652ebcb7b38830bf10447a722cf07",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "ef8906634fadd400d0a9375eb5395e280641d231130dc9423a14560287869976",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-char-to-utf8-varchar-width4": {
      "metadata": "d07af4e7778540ce7675d67742d26ead548652ebcb7b38830bf10447a722cf07",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "66ce6776da8a692393534daee4ae6b89de0ee3f3da972559db0afafcfae88207",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-char-to-utf8-char-width3": {
      "metadata": "7511bae1df7d8cf250f3c42f7ca1a90ebd8ff7e2a06a2caedabc906a59c7b857",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "60d96a81c74b29377f111cb82d9fd00189640d0245d3fe2b57bc8e642b3648cc",
      "readback": "12d0d3be860259268c2896f832f709d967f55a5eb9d8f41c12b48c4435d7a1dc",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-char-to-utf8-char-width4": {
      "metadata": "7511bae1df7d8cf250f3c42f7ca1a90ebd8ff7e2a06a2caedabc906a59c7b857",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "06dcbdcb34477c194c945cc3c52fa38babd45fedea00b34cfcf723027d6250b1",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-char-to-unicode-nvarchar-width3": {
      "metadata": "d07af4e7778540ce7675d67742d26ead548652ebcb7b38830bf10447a722cf07",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "227f60091a4d23db74a00e3273f95f6988e25fa8ef8788ff0bc36af58f96c18e",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-char-to-unicode-nvarchar-width4": {
      "metadata": "d07af4e7778540ce7675d67742d26ead548652ebcb7b38830bf10447a722cf07",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "b0c9cacd368ab6e986fca55e07a1e5d8239bc72e8ae3237f4797add732864a66",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-char-to-unicode-nchar-width3": {
      "metadata": "d07af4e7778540ce7675d67742d26ead548652ebcb7b38830bf10447a722cf07",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "83f87d6b77b30576bc1f118b6255ffc804022d0a63f2f63176e47a6a6275306d",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-char-to-unicode-nchar-width4": {
      "metadata": "d07af4e7778540ce7675d67742d26ead548652ebcb7b38830bf10447a722cf07",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "76f85362bf20067973947df68496d0b19b99e56a93e8358b66ed1383349a2b00",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-varchar-to-cp1252-varchar-width1": {
      "metadata": "8fc3a2ba5c2b56655243888dfcd334f0dc6a752cd4a0b78de1c8758d21e5a83a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "88e15f3eadaec0515ea1b7976b85e154a7339a4236c51c960482f743a3f80ddf",
      "readback": "85dbd7d3c02d099115d90e0eb3a2e02b9b2ad88f4baa503a5cc7d26175fd2284",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-varchar-to-cp1252-varchar-width2": {
      "metadata": "8fc3a2ba5c2b56655243888dfcd334f0dc6a752cd4a0b78de1c8758d21e5a83a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "bbf53afd1bcd48664eb8f7a122047f92c8e21063adee14bf9cdc4c034f2a9a76",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-varchar-to-cp1252-char-width3": {
      "metadata": "9aa9d3b9b3fc09ff3dee2a5e443a254a26e9fba2213a46ad79cf523a3ed3010a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "059e9f8a662d7e54c8d6a9e6d1deb7abcdd68c9725e9cf153c071fb497b8aa56",
      "readback": "9226eb3cd890deed19d0530dc4008f05a110aba216dfb99297b6fe002c26910f",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-varchar-to-cp1252-char-width4": {
      "metadata": "9aa9d3b9b3fc09ff3dee2a5e443a254a26e9fba2213a46ad79cf523a3ed3010a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "ebd8bb2625659161d8a4f7f5507b437b9bd28ecff5256445d086219516c2ec81",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-varchar-to-utf8-varchar-width3": {
      "metadata": "9aa9d3b9b3fc09ff3dee2a5e443a254a26e9fba2213a46ad79cf523a3ed3010a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4d6f8b51a0c776732ea6e10cb29e3da4ea6890969792df34852e011e269b64e6",
      "readback": "1f0a1d4ab060d5a418fbdd4c9077f1835cb7398164cf71288290a3882644acf3",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-varchar-to-utf8-varchar-width4": {
      "metadata": "9aa9d3b9b3fc09ff3dee2a5e443a254a26e9fba2213a46ad79cf523a3ed3010a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "617ed3c368418e27892ae338bb9090fdf9dc76b2e0257989b32848951a0e74f7",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-varchar-to-utf8-char-width2": {
      "metadata": "46584fbfaf503cc4abcc4213b40f5a4d0c166d3a6ed4a06c09da52760ef5cb20",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "22bc1307ca76a32ffdd6acf6f70cb5e5f5d144c6a0b05b6b9df2e8cc4ed0017a",
      "readback": "703de746602ce8cde00bc32b71b70dd69bfd747cb7fda4253c79ba57c0636098",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-varchar-to-utf8-char-width3": {
      "metadata": "46584fbfaf503cc4abcc4213b40f5a4d0c166d3a6ed4a06c09da52760ef5cb20",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "77512380d66829816f284a8f51996d2769a0b19167359ae37f1ad68c8f5e3c3e",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-varchar-to-unicode-nvarchar-width1": {
      "metadata": "46584fbfaf503cc4abcc4213b40f5a4d0c166d3a6ed4a06c09da52760ef5cb20",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "503efbfb3f5ccedc5555fd2e8454ab16284c780133fba2641d475395dd80d75c",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-row2-declared1-wire1": {
      "metadata": "99b9412231b7b488cdaf15eab6ac292c58b59b7780e74ae91b8d31ea03bd9c58",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "ca542dfa117a13196067eeb099b8931e2342db67abb3ea9677fc53bc5e851308",
      "readback": "8cd12242dbff31d06b8a46817160b85c16277eb7ffac5357b3168a0717ca32a3",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-varchar-to-unicode-nchar-width1": {
      "metadata": "9aa9d3b9b3fc09ff3dee2a5e443a254a26e9fba2213a46ad79cf523a3ed3010a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4f05d7913a929e4f7fdbabd6fd0ece9fb8746ff4055b0afbbe503d17b7119ba6",
      "readback": "cc6c27a196eab2b7d479ccc0efb6aa7f1072aaea469537ca23ea438b5c3386ab",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-varchar-to-unicode-nchar-width2": {
      "metadata": "9aa9d3b9b3fc09ff3dee2a5e443a254a26e9fba2213a46ad79cf523a3ed3010a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "3bc0d4adf89098ba8ba51e9a2098bd019dbb0bab221f6eee0a1555fe248d777e",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-char-to-cp1252-varchar-width1": {
      "metadata": "4784bc2cd7d1425d1c56c81850fe4021c5edd578b8aa7fe355b1f345d27c6f78",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "88e15f3eadaec0515ea1b7976b85e154a7339a4236c51c960482f743a3f80ddf",
      "readback": "85dbd7d3c02d099115d90e0eb3a2e02b9b2ad88f4baa503a5cc7d26175fd2284",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-char-to-cp1252-varchar-width2": {
      "metadata": "4784bc2cd7d1425d1c56c81850fe4021c5edd578b8aa7fe355b1f345d27c6f78",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "5b2d0078de4713cac4d157ebbaeaeb7b5f7daa38522157716e8d3174d0f27b1a",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-char-to-cp1252-char-width3": {
      "metadata": "390019cce2ba552fee1ad2a99bd01795d01c7215623c12212ecf1b5b3d9ee589",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "059e9f8a662d7e54c8d6a9e6d1deb7abcdd68c9725e9cf153c071fb497b8aa56",
      "readback": "9226eb3cd890deed19d0530dc4008f05a110aba216dfb99297b6fe002c26910f",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-char-to-cp1252-char-width4": {
      "metadata": "390019cce2ba552fee1ad2a99bd01795d01c7215623c12212ecf1b5b3d9ee589",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "544be55ee1fd9c4a4f4d1f27c1da9e911e3ac52633ab9aceecc0756042c282ac",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-char-to-utf8-varchar-width3": {
      "metadata": "390019cce2ba552fee1ad2a99bd01795d01c7215623c12212ecf1b5b3d9ee589",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4d6f8b51a0c776732ea6e10cb29e3da4ea6890969792df34852e011e269b64e6",
      "readback": "1f0a1d4ab060d5a418fbdd4c9077f1835cb7398164cf71288290a3882644acf3",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-char-to-utf8-varchar-width4": {
      "metadata": "390019cce2ba552fee1ad2a99bd01795d01c7215623c12212ecf1b5b3d9ee589",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "9acdc466dd89a17865a647c8a42a19194b6be61ca5058afc3de50f8002dab283",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-char-to-utf8-char-width2": {
      "metadata": "6beeff74b9cfeaa4ccfba2f94a519658b37c97db8a093159c3d283a0dae4189e",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "22bc1307ca76a32ffdd6acf6f70cb5e5f5d144c6a0b05b6b9df2e8cc4ed0017a",
      "readback": "703de746602ce8cde00bc32b71b70dd69bfd747cb7fda4253c79ba57c0636098",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-char-to-utf8-char-width3": {
      "metadata": "6beeff74b9cfeaa4ccfba2f94a519658b37c97db8a093159c3d283a0dae4189e",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "fc776a8b027af8546b68ed7241197ee9784634466b91e48c88fa6220ff42ea89",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-char-to-unicode-nvarchar-width1": {
      "metadata": "6beeff74b9cfeaa4ccfba2f94a519658b37c97db8a093159c3d283a0dae4189e",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "751e1bf4f1fa2a0b578cd438ec6edf5bb99c687b6f262bdabfc30bff1116bdc5",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-char-to-unicode-nvarchar-width2": {
      "metadata": "6beeff74b9cfeaa4ccfba2f94a519658b37c97db8a093159c3d283a0dae4189e",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "8029746dec99a95feddc5a2dce985c22a2603f73717b8f2fa70e75a1f294f96d",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-char-to-unicode-nchar-width1": {
      "metadata": "390019cce2ba552fee1ad2a99bd01795d01c7215623c12212ecf1b5b3d9ee589",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4f05d7913a929e4f7fdbabd6fd0ece9fb8746ff4055b0afbbe503d17b7119ba6",
      "readback": "cc6c27a196eab2b7d479ccc0efb6aa7f1072aaea469537ca23ea438b5c3386ab",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-char-to-unicode-nchar-width2": {
      "metadata": "390019cce2ba552fee1ad2a99bd01795d01c7215623c12212ecf1b5b3d9ee589",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "e6a06502443e8e2b75f361bdfd7593c06ff773b9b2f136936147cee789cc447d",
      "readback": "989e9f43a8761148083530a5bb21c4c6a19f4d4a2770e2c602054d8b21b57a40",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-varchar-declared1-wire2": {
      "metadata": "0c4fb211bfa69b909882ba384f001d35ee79cb0488205d92888e225c2fd1a4df",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "20701f37a03b2e7d2efb1bd976512d76b269a78f294d9d8da75dfd59302d86fd",
      "readback": "bd09817000af8bd022857c8328210826fed5fe5ceed38bb22343d224f419a875",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-varchar-declared2-wire1": {
      "metadata": "3b25cd93d7026f5fc2fb033a92fd2c08e9a0369d136f34c7d5785a822bfe177b",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "20701f37a03b2e7d2efb1bd976512d76b269a78f294d9d8da75dfd59302d86fd",
      "readback": "bd09817000af8bd022857c8328210826fed5fe5ceed38bb22343d224f419a875",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-char-declared1-wire2": {
      "metadata": "0a8c1e8c07e6731e5c825712898e66879abe643e77f02d91bd11988c6c15eef1",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "20701f37a03b2e7d2efb1bd976512d76b269a78f294d9d8da75dfd59302d86fd",
      "readback": "bd09817000af8bd022857c8328210826fed5fe5ceed38bb22343d224f419a875",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-char-declared2-wire1": {
      "metadata": "2ca2a4237fd04c8f435a4e75c192ee6002f655dccc89af79b179a464aa3cccf7",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "20701f37a03b2e7d2efb1bd976512d76b269a78f294d9d8da75dfd59302d86fd",
      "readback": "bd09817000af8bd022857c8328210826fed5fe5ceed38bb22343d224f419a875",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-varchar-declared1-wire2": {
      "metadata": "701b639a40d068cb318638b522868b955527e3b368006e86e835a387b250410a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "20701f37a03b2e7d2efb1bd976512d76b269a78f294d9d8da75dfd59302d86fd",
      "readback": "bd09817000af8bd022857c8328210826fed5fe5ceed38bb22343d224f419a875",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-varchar-declared2-wire1": {
      "metadata": "d8d09e9e070397797aee0d23bc2809e1fd9af51d295cc0fc047f9fe79a11acf6",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "20701f37a03b2e7d2efb1bd976512d76b269a78f294d9d8da75dfd59302d86fd",
      "readback": "bd09817000af8bd022857c8328210826fed5fe5ceed38bb22343d224f419a875",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-char-declared1-wire2": {
      "metadata": "d07af4e7778540ce7675d67742d26ead548652ebcb7b38830bf10447a722cf07",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "20701f37a03b2e7d2efb1bd976512d76b269a78f294d9d8da75dfd59302d86fd",
      "readback": "bd09817000af8bd022857c8328210826fed5fe5ceed38bb22343d224f419a875",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-char-declared2-wire1": {
      "metadata": "27bcd92e99aa599bd759aa54206e346519e890938beebe8bff47ec878dd9c3f3",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "20701f37a03b2e7d2efb1bd976512d76b269a78f294d9d8da75dfd59302d86fd",
      "readback": "bd09817000af8bd022857c8328210826fed5fe5ceed38bb22343d224f419a875",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-varchar-declared1-wire2": {
      "metadata": "8fc3a2ba5c2b56655243888dfcd334f0dc6a752cd4a0b78de1c8758d21e5a83a",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "20701f37a03b2e7d2efb1bd976512d76b269a78f294d9d8da75dfd59302d86fd",
      "readback": "bd09817000af8bd022857c8328210826fed5fe5ceed38bb22343d224f419a875",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-varchar-declared2-wire1": {
      "metadata": "5699bd8a1049e17da64a97f660eab9ee793420984d97d4ce8449b7428a78a90f",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "20701f37a03b2e7d2efb1bd976512d76b269a78f294d9d8da75dfd59302d86fd",
      "readback": "bd09817000af8bd022857c8328210826fed5fe5ceed38bb22343d224f419a875",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-char-declared1-wire2": {
      "metadata": "4784bc2cd7d1425d1c56c81850fe4021c5edd578b8aa7fe355b1f345d27c6f78",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "20701f37a03b2e7d2efb1bd976512d76b269a78f294d9d8da75dfd59302d86fd",
      "readback": "bd09817000af8bd022857c8328210826fed5fe5ceed38bb22343d224f419a875",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-char-declared2-wire1": {
      "metadata": "99b9412231b7b488cdaf15eab6ac292c58b59b7780e74ae91b8d31ea03bd9c58",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "20701f37a03b2e7d2efb1bd976512d76b269a78f294d9d8da75dfd59302d86fd",
      "readback": "bd09817000af8bd022857c8328210826fed5fe5ceed38bb22343d224f419a875",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-char-null-empty-to-char4": {
      "metadata": "2ca2a4237fd04c8f435a4e75c192ee6002f655dccc89af79b179a464aa3cccf7",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "7878a33babf25c1fa696f01973de5cacbf4c0b667dff94d95c73586d40363077",
      "readback": "eba89f2e12f2f17334ec08b68793c424e41f5daffd4e0d520f7859c6ada26884",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-char-null-empty-to-nchar4": {
      "metadata": "2ca2a4237fd04c8f435a4e75c192ee6002f655dccc89af79b179a464aa3cccf7",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "7878a33babf25c1fa696f01973de5cacbf4c0b667dff94d95c73586d40363077",
      "readback": "3e3ec104011a1cb499a113d0d1e6fa264df7309521f936c9b8b8916f6fc872a1",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-char-null-empty-to-char4": {
      "metadata": "27bcd92e99aa599bd759aa54206e346519e890938beebe8bff47ec878dd9c3f3",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "7878a33babf25c1fa696f01973de5cacbf4c0b667dff94d95c73586d40363077",
      "readback": "eba89f2e12f2f17334ec08b68793c424e41f5daffd4e0d520f7859c6ada26884",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-char-null-empty-to-nchar4": {
      "metadata": "27bcd92e99aa599bd759aa54206e346519e890938beebe8bff47ec878dd9c3f3",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "7878a33babf25c1fa696f01973de5cacbf4c0b667dff94d95c73586d40363077",
      "readback": "3e3ec104011a1cb499a113d0d1e6fa264df7309521f936c9b8b8916f6fc872a1",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-char-null-empty-to-char4": {
      "metadata": "99b9412231b7b488cdaf15eab6ac292c58b59b7780e74ae91b8d31ea03bd9c58",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "7878a33babf25c1fa696f01973de5cacbf4c0b667dff94d95c73586d40363077",
      "readback": "eba89f2e12f2f17334ec08b68793c424e41f5daffd4e0d520f7859c6ada26884",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-char-null-empty-to-nchar4": {
      "metadata": "99b9412231b7b488cdaf15eab6ac292c58b59b7780e74ae91b8d31ea03bd9c58",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "7878a33babf25c1fa696f01973de5cacbf4c0b667dff94d95c73586d40363077",
      "readback": "3e3ec104011a1cb499a113d0d1e6fa264df7309521f936c9b8b8916f6fc872a1",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1251-max-to-utf8-varchar-max": {
      "metadata": "be3d55463049f9177e9dc1cda724e1e3652e4b55dbf76ab5f00f6233b2a387bc",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "ccb7325c7ea1f205c560de9456c7a9716f7428c21eb4917ced1873859c9a4de4",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "cp1252-max-to-utf8-varchar-max": {
      "metadata": "102f588e6a99a3002efb8e7a760bcd5b8497a8072d294229879ef94273790706",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "2ca999cc775dd1566a6f2505882d9ad34b87b30c58c30e935a5a10b3298da489",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-max-to-cp1252-varchar-max": {
      "metadata": "9ddc4886a06984a012c95944423f059468a1ce1848b47c5416f48934c823b76b",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "6f8ca440cd719625e4972f2b1361af1e1458ec0aadf5e5d0487ea60e9169b5f4",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-max-to-unicode-nvarchar-max": {
      "metadata": "9ddc4886a06984a012c95944423f059468a1ce1848b47c5416f48934c823b76b",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "8182513e8c591e2f575a884b698ea957f13871d0bc73e2e6b76c8ba87dfdf5d9",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-euro-to-cp1252-varchar-width1": {
      "metadata": "46584fbfaf503cc4abcc4213b40f5a4d0c166d3a6ed4a06c09da52760ef5cb20",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "30b4dbc80e4141b19a79e616ba78220fd36e6cd9c8d7d6b74fabacc882c40431",
      "readback": "85dbd7d3c02d099115d90e0eb3a2e02b9b2ad88f4baa503a5cc7d26175fd2284",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    },
    "utf8-euro-to-cp1252-varchar-width3": {
      "metadata": "46584fbfaf503cc4abcc4213b40f5a4d0c166d3a6ed4a06c09da52760ef5cb20",
      "setup": "0e31de0385c07455d0260e83a11b66c884997ecce063b5bd8dd00e2195203fca",
      "execution": "4b1e80e01ab7823500e082c0850248545e3a508caa0e6a530df2645e484e5ca0",
      "readback": "956d784fbbe5df4567b500765b6c314c1453db2351731e74331dfac009ae55a6",
      "cleanup": "224aefcffdd8c15de62184740c22f8c530eb2a7f842d7b6be1800ff6c5e7a745"
    }
  },
  "fragmentation": {
    "cp1251-varchar-to-cp1252-varchar-width1": {
      "metadata": "fb5cba75bac89fb8f3963492f8f00e46560dee9d40539a08d4909e22358cf18e",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "8c18cdfd181721d3e2d9d2d7486aec5539f4cbe545a5b943e7010b8cc27f125d",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-varchar-to-cp1252-varchar-width2": {
      "metadata": "fb5cba75bac89fb8f3963492f8f00e46560dee9d40539a08d4909e22358cf18e",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "13a313868bfd6a665e4bfe4a0f97dab7a310c934005148622de535fd3c172ae3",
      "readback": "95a543c13435189c211ea1f374c8911cc02a5e8b5082bfd7bdde9ab256af4f84",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-varchar-to-cp1252-char-width1": {
      "metadata": "fb5cba75bac89fb8f3963492f8f00e46560dee9d40539a08d4909e22358cf18e",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "8c18cdfd181721d3e2d9d2d7486aec5539f4cbe545a5b943e7010b8cc27f125d",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-varchar-to-cp1252-char-width2": {
      "metadata": "fb5cba75bac89fb8f3963492f8f00e46560dee9d40539a08d4909e22358cf18e",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "13a313868bfd6a665e4bfe4a0f97dab7a310c934005148622de535fd3c172ae3",
      "readback": "0c7f7bb9bd68c7b43bc2d1824688b7dbbbe302512d34a15bf5f8e9c4ab6a645b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-varchar-to-utf8-varchar-width1": {
      "metadata": "fb5cba75bac89fb8f3963492f8f00e46560dee9d40539a08d4909e22358cf18e",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "f3f2be110c60f9f5bbb9c1ae2c923efd12420ec17fad2f576ae8bec26c14088f",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-varchar-to-utf8-varchar-width2": {
      "metadata": "fb5cba75bac89fb8f3963492f8f00e46560dee9d40539a08d4909e22358cf18e",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "13a313868bfd6a665e4bfe4a0f97dab7a310c934005148622de535fd3c172ae3",
      "readback": "a539a07742800bd80aac57b526d1d70d1f528cb541b9f95eb2bea53aaaa7a9de",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-varchar-to-utf8-char-width1": {
      "metadata": "fb5cba75bac89fb8f3963492f8f00e46560dee9d40539a08d4909e22358cf18e",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "3954f06a16aea5f31bda9253a498915d96a61896b74fcdbeec93973f46abef4f",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-varchar-to-utf8-char-width2": {
      "metadata": "fb5cba75bac89fb8f3963492f8f00e46560dee9d40539a08d4909e22358cf18e",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "00e7a52857bc4203521bf84654f5020bd71617d4f02dd2b26304b5ed0d085b86",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-varchar-to-unicode-nvarchar-width1": {
      "metadata": "fb5cba75bac89fb8f3963492f8f00e46560dee9d40539a08d4909e22358cf18e",
      "setup": "fbd5b3a34c9f5880de6f8fc1db1814392ca7d32add6b3ad6017aa0e0e73569b5",
      "execution": "8c18cdfd181721d3e2d9d2d7486aec5539f4cbe545a5b943e7010b8cc27f125d",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-varchar-to-unicode-nvarchar-width2": {
      "metadata": "fb5cba75bac89fb8f3963492f8f00e46560dee9d40539a08d4909e22358cf18e",
      "setup": "fbd5b3a34c9f5880de6f8fc1db1814392ca7d32add6b3ad6017aa0e0e73569b5",
      "execution": "13a313868bfd6a665e4bfe4a0f97dab7a310c934005148622de535fd3c172ae3",
      "readback": "49c8c34130b7da5b4c73dc53bfe34370586b588b01b6248f798f13b7b17cbb1e",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-varchar-to-unicode-nchar-width1": {
      "metadata": "fb5cba75bac89fb8f3963492f8f00e46560dee9d40539a08d4909e22358cf18e",
      "setup": "3b96f1db226707756dc755d2ac70581f8c3d1635d6e4dd5013bf2348d460d26f",
      "execution": "8c18cdfd181721d3e2d9d2d7486aec5539f4cbe545a5b943e7010b8cc27f125d",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-varchar-to-unicode-nchar-width2": {
      "metadata": "fb5cba75bac89fb8f3963492f8f00e46560dee9d40539a08d4909e22358cf18e",
      "setup": "3b96f1db226707756dc755d2ac70581f8c3d1635d6e4dd5013bf2348d460d26f",
      "execution": "13a313868bfd6a665e4bfe4a0f97dab7a310c934005148622de535fd3c172ae3",
      "readback": "c238c7ee98234e5759cc7e596f2109f75402303be04709bc717dba1d2b16c17f",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-char-to-cp1252-varchar-width1": {
      "metadata": "474a37df74d037af59ebf3a736b2acfba29b34bdd0129265b25b402f9d0820bd",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "0d3e1715988d6d90765de3ac043cf80c309d1c9e6b10cf6ffe83a51593007e1a",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-char-to-cp1252-varchar-width2": {
      "metadata": "474a37df74d037af59ebf3a736b2acfba29b34bdd0129265b25b402f9d0820bd",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "cedab488078784d6a1fa3b5e904a6b93a5e7384594068bd3dff7bbe192cdf026",
      "readback": "084fc5faf130a99c443f334864f05fd2031b23b19388471355044c2744f67c2b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-char-to-cp1252-char-width1": {
      "metadata": "474a37df74d037af59ebf3a736b2acfba29b34bdd0129265b25b402f9d0820bd",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "0d3e1715988d6d90765de3ac043cf80c309d1c9e6b10cf6ffe83a51593007e1a",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-char-to-cp1252-char-width2": {
      "metadata": "474a37df74d037af59ebf3a736b2acfba29b34bdd0129265b25b402f9d0820bd",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "cedab488078784d6a1fa3b5e904a6b93a5e7384594068bd3dff7bbe192cdf026",
      "readback": "084fc5faf130a99c443f334864f05fd2031b23b19388471355044c2744f67c2b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-char-to-utf8-varchar-width1": {
      "metadata": "474a37df74d037af59ebf3a736b2acfba29b34bdd0129265b25b402f9d0820bd",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "40e66b5cc47fbaf59f7c95c1b29806e8c018e10b717b51acbbb42cbae03b7bf5",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-char-to-utf8-varchar-width2": {
      "metadata": "474a37df74d037af59ebf3a736b2acfba29b34bdd0129265b25b402f9d0820bd",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "cedab488078784d6a1fa3b5e904a6b93a5e7384594068bd3dff7bbe192cdf026",
      "readback": "b0858a39c05b6dac21929c4d021b12a4c7a73bc50cf1f6beaf2f2bd7be044915",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-char-to-utf8-char-width1": {
      "metadata": "04f8050edbafafa47a1724d610dce16cc782bd12a13aeeaaca9645577a70b93f",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "de94e48d492ae6a5f50fc4caab71a97b3e87721485db91750385139fc1ac8581",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-char-to-utf8-char-width2": {
      "metadata": "04f8050edbafafa47a1724d610dce16cc782bd12a13aeeaaca9645577a70b93f",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "89f55212cc6df28f861a73bc56bc3691d4b7ff6a12cd8fab041ae76b1e892d8c",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-char-to-unicode-nvarchar-width1": {
      "metadata": "474a37df74d037af59ebf3a736b2acfba29b34bdd0129265b25b402f9d0820bd",
      "setup": "fbd5b3a34c9f5880de6f8fc1db1814392ca7d32add6b3ad6017aa0e0e73569b5",
      "execution": "0d3e1715988d6d90765de3ac043cf80c309d1c9e6b10cf6ffe83a51593007e1a",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-char-to-unicode-nvarchar-width2": {
      "metadata": "474a37df74d037af59ebf3a736b2acfba29b34bdd0129265b25b402f9d0820bd",
      "setup": "fbd5b3a34c9f5880de6f8fc1db1814392ca7d32add6b3ad6017aa0e0e73569b5",
      "execution": "cedab488078784d6a1fa3b5e904a6b93a5e7384594068bd3dff7bbe192cdf026",
      "readback": "be8208f04b978e5793d46180d044b783fb864b8e29dad71390e95e27881535b3",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-char-to-unicode-nchar-width1": {
      "metadata": "474a37df74d037af59ebf3a736b2acfba29b34bdd0129265b25b402f9d0820bd",
      "setup": "3b96f1db226707756dc755d2ac70581f8c3d1635d6e4dd5013bf2348d460d26f",
      "execution": "0d3e1715988d6d90765de3ac043cf80c309d1c9e6b10cf6ffe83a51593007e1a",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-char-to-unicode-nchar-width2": {
      "metadata": "474a37df74d037af59ebf3a736b2acfba29b34bdd0129265b25b402f9d0820bd",
      "setup": "3b96f1db226707756dc755d2ac70581f8c3d1635d6e4dd5013bf2348d460d26f",
      "execution": "cedab488078784d6a1fa3b5e904a6b93a5e7384594068bd3dff7bbe192cdf026",
      "readback": "be8208f04b978e5793d46180d044b783fb864b8e29dad71390e95e27881535b3",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-varchar-to-cp1252-varchar-width3": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "bb5281ec2d98bda5f18b1bc080a01c5d4f4c8448db29b3250d72dcdd72337b9f",
      "readback": "95a543c13435189c211ea1f374c8911cc02a5e8b5082bfd7bdde9ab256af4f84",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-varchar-to-cp1252-varchar-width4": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "bb5281ec2d98bda5f18b1bc080a01c5d4f4c8448db29b3250d72dcdd72337b9f",
      "readback": "95a543c13435189c211ea1f374c8911cc02a5e8b5082bfd7bdde9ab256af4f84",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-varchar-to-cp1252-char-width3": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "bb5281ec2d98bda5f18b1bc080a01c5d4f4c8448db29b3250d72dcdd72337b9f",
      "readback": "139111c1c437ab4e23876cefa163c6745322d2f1182556aec5078e4315fb15dd",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-varchar-to-cp1252-char-width4": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "bb5281ec2d98bda5f18b1bc080a01c5d4f4c8448db29b3250d72dcdd72337b9f",
      "readback": "ec50f2f3e05baddc653e7f09beaedf60224908e45566f25592820f5bd6785ecd",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-varchar-to-utf8-varchar-width3": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "bb5281ec2d98bda5f18b1bc080a01c5d4f4c8448db29b3250d72dcdd72337b9f",
      "readback": "a539a07742800bd80aac57b526d1d70d1f528cb541b9f95eb2bea53aaaa7a9de",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-varchar-to-utf8-varchar-width4": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "bb5281ec2d98bda5f18b1bc080a01c5d4f4c8448db29b3250d72dcdd72337b9f",
      "readback": "349f15179e7455ec79d738bafd884a6879838035127661b7b02b6f327accd67e",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-varchar-to-utf8-char-width3": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "233cb6a9c7fde2aedbfd4c191da46977856a4483bb47bd7ed2f0dd95640bc8ff",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-varchar-to-utf8-char-width4": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "991cdc2d5c908070d4fb0f67f616aab6dcf4b399f632ff4f1fadbc1119b18e14",
      "readback": "bb5034b960e39549a874f26f632ee33a95374ccb5f19e010a9b7ddaee73e207b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-varchar-to-unicode-nvarchar-width3": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "fbd5b3a34c9f5880de6f8fc1db1814392ca7d32add6b3ad6017aa0e0e73569b5",
      "execution": "bb5281ec2d98bda5f18b1bc080a01c5d4f4c8448db29b3250d72dcdd72337b9f",
      "readback": "49c8c34130b7da5b4c73dc53bfe34370586b588b01b6248f798f13b7b17cbb1e",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-row2-declared1-wire1": {
      "metadata": "7de6b0ea33d80b595d994baf22fe9ed7521c9c47e2660259ce0d99f3de5d183d",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "e61efe253350492b941d0b802dae6173d088185a8fc8211ed0de9b7ebca49c72",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-varchar-to-unicode-nchar-width3": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "3b96f1db226707756dc755d2ac70581f8c3d1635d6e4dd5013bf2348d460d26f",
      "execution": "bb5281ec2d98bda5f18b1bc080a01c5d4f4c8448db29b3250d72dcdd72337b9f",
      "readback": "ec50f2f3e05baddc653e7f09beaedf60224908e45566f25592820f5bd6785ecd",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-varchar-to-unicode-nchar-width4": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "3b96f1db226707756dc755d2ac70581f8c3d1635d6e4dd5013bf2348d460d26f",
      "execution": "bb5281ec2d98bda5f18b1bc080a01c5d4f4c8448db29b3250d72dcdd72337b9f",
      "readback": "e439bacaec2e6eddc01a7bfca4460d0abe9b91749e0555b3fd6a393067fa71ab",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-char-to-cp1252-varchar-width3": {
      "metadata": "2a31c3edebe6f9e3bf53592ed4589afd9e411deadf749fd867ddd6c2f38e9e4b",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "e376eef60343c7647ac2193644bad0f76dec0b6e42eec03ad33490bb0ea53712",
      "readback": "084fc5faf130a99c443f334864f05fd2031b23b19388471355044c2744f67c2b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-char-to-cp1252-varchar-width4": {
      "metadata": "2a31c3edebe6f9e3bf53592ed4589afd9e411deadf749fd867ddd6c2f38e9e4b",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "e376eef60343c7647ac2193644bad0f76dec0b6e42eec03ad33490bb0ea53712",
      "readback": "084fc5faf130a99c443f334864f05fd2031b23b19388471355044c2744f67c2b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-char-to-cp1252-char-width3": {
      "metadata": "2a31c3edebe6f9e3bf53592ed4589afd9e411deadf749fd867ddd6c2f38e9e4b",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "e376eef60343c7647ac2193644bad0f76dec0b6e42eec03ad33490bb0ea53712",
      "readback": "0280af3be712512f2747c268549380c0ac088aa6324a5b45c81017aceb04c372",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-char-to-cp1252-char-width4": {
      "metadata": "2a31c3edebe6f9e3bf53592ed4589afd9e411deadf749fd867ddd6c2f38e9e4b",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "e376eef60343c7647ac2193644bad0f76dec0b6e42eec03ad33490bb0ea53712",
      "readback": "73fff24ffda82eb94b14eb1dab159c0434f45ce9a34969eed213f03f90637b18",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-char-to-utf8-varchar-width3": {
      "metadata": "2a31c3edebe6f9e3bf53592ed4589afd9e411deadf749fd867ddd6c2f38e9e4b",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "e376eef60343c7647ac2193644bad0f76dec0b6e42eec03ad33490bb0ea53712",
      "readback": "b0858a39c05b6dac21929c4d021b12a4c7a73bc50cf1f6beaf2f2bd7be044915",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-char-to-utf8-varchar-width4": {
      "metadata": "2a31c3edebe6f9e3bf53592ed4589afd9e411deadf749fd867ddd6c2f38e9e4b",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "e376eef60343c7647ac2193644bad0f76dec0b6e42eec03ad33490bb0ea53712",
      "readback": "be8208f04b978e5793d46180d044b783fb864b8e29dad71390e95e27881535b3",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-char-to-utf8-char-width3": {
      "metadata": "214100c83044b0af77468ec8ec6dcad193c94dd82966948ae65347d6c4345676",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "361d95c5b15bbe694764d796f94d120085f1ee39a884a88eab772617b495d2e1",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-char-to-utf8-char-width4": {
      "metadata": "214100c83044b0af77468ec8ec6dcad193c94dd82966948ae65347d6c4345676",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "3c8b04ceed430bd8b7bbeefb0404aeadbd963e07234d54f8434cae273eb798e0",
      "readback": "77d17c6a384a285f8ba50d5582ee120b81930af5834f20c70f090c16793302f1",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-char-to-unicode-nvarchar-width3": {
      "metadata": "2a31c3edebe6f9e3bf53592ed4589afd9e411deadf749fd867ddd6c2f38e9e4b",
      "setup": "fbd5b3a34c9f5880de6f8fc1db1814392ca7d32add6b3ad6017aa0e0e73569b5",
      "execution": "e376eef60343c7647ac2193644bad0f76dec0b6e42eec03ad33490bb0ea53712",
      "readback": "be8208f04b978e5793d46180d044b783fb864b8e29dad71390e95e27881535b3",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-char-to-unicode-nvarchar-width4": {
      "metadata": "2a31c3edebe6f9e3bf53592ed4589afd9e411deadf749fd867ddd6c2f38e9e4b",
      "setup": "fbd5b3a34c9f5880de6f8fc1db1814392ca7d32add6b3ad6017aa0e0e73569b5",
      "execution": "e376eef60343c7647ac2193644bad0f76dec0b6e42eec03ad33490bb0ea53712",
      "readback": "be8208f04b978e5793d46180d044b783fb864b8e29dad71390e95e27881535b3",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-char-to-unicode-nchar-width3": {
      "metadata": "2a31c3edebe6f9e3bf53592ed4589afd9e411deadf749fd867ddd6c2f38e9e4b",
      "setup": "3b96f1db226707756dc755d2ac70581f8c3d1635d6e4dd5013bf2348d460d26f",
      "execution": "e376eef60343c7647ac2193644bad0f76dec0b6e42eec03ad33490bb0ea53712",
      "readback": "73fff24ffda82eb94b14eb1dab159c0434f45ce9a34969eed213f03f90637b18",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-char-to-unicode-nchar-width4": {
      "metadata": "2a31c3edebe6f9e3bf53592ed4589afd9e411deadf749fd867ddd6c2f38e9e4b",
      "setup": "3b96f1db226707756dc755d2ac70581f8c3d1635d6e4dd5013bf2348d460d26f",
      "execution": "e376eef60343c7647ac2193644bad0f76dec0b6e42eec03ad33490bb0ea53712",
      "readback": "f6a4f756610878007f8deab430502b3ec055d3624afdb818cb4cfc47d7c34c4c",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-varchar-to-cp1252-varchar-width1": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "9a02aea0eb459806738763e5098b179f5ad634463d0c4c86a94eb12cba10e3a0",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-varchar-to-cp1252-varchar-width2": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "bb5281ec2d98bda5f18b1bc080a01c5d4f4c8448db29b3250d72dcdd72337b9f",
      "readback": "ed94624a2a5ff3f09a4a9744b55116b726ec55b81ee486c9864c1a1e6b9882c6",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-varchar-to-cp1252-char-width3": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "c4b509c73b45d6d14fc4dc7d49dbf13f16af30b9c4b23646e1ff8b441ada19a7",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-varchar-to-cp1252-char-width4": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "991cdc2d5c908070d4fb0f67f616aab6dcf4b399f632ff4f1fadbc1119b18e14",
      "readback": "ec50f2f3e05baddc653e7f09beaedf60224908e45566f25592820f5bd6785ecd",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-varchar-to-utf8-varchar-width3": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "fbaaa278d533131f8b925b9966c2b4ca7b0c990fff61470f4775ae52eede25c0",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-varchar-to-utf8-varchar-width4": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "991cdc2d5c908070d4fb0f67f616aab6dcf4b399f632ff4f1fadbc1119b18e14",
      "readback": "349f15179e7455ec79d738bafd884a6879838035127661b7b02b6f327accd67e",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-varchar-to-utf8-char-width2": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "0aa6912d9018643bacdb39608a851d9fba8edaea252f8cbcdf9dde207fbc764d",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-varchar-to-utf8-char-width3": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "8cd0d41ee9f59071624ecc9f16aebe0f5cbdc01ffe3f684e60258dbbf244dac2",
      "readback": "c4af7eca085e4ee779e6b4f0a2db5fa53557b7e4d50239c862595c497a825a2b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-varchar-to-unicode-nvarchar-width1": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "fbd5b3a34c9f5880de6f8fc1db1814392ca7d32add6b3ad6017aa0e0e73569b5",
      "execution": "8cd0d41ee9f59071624ecc9f16aebe0f5cbdc01ffe3f684e60258dbbf244dac2",
      "readback": "dd4326b26bec75394e19f7cba44e7886f76ce942e8c6434061c0fd4fc35eeb64",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-row2-declared1-wire1": {
      "metadata": "f622d3838f490b4d7d08b0173c35f76430fd75645d9129f376a0e54249d3ef39",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "61a45be8e78b343be74a15b97a931bfaf78424c6da2accd9de7109a1192b8f12",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-varchar-to-unicode-nchar-width1": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "3b96f1db226707756dc755d2ac70581f8c3d1635d6e4dd5013bf2348d460d26f",
      "execution": "43ab920a8337277d44ed34d0f05e92a23fa25415a797cfb47c45683295f8057f",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-varchar-to-unicode-nchar-width2": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "3b96f1db226707756dc755d2ac70581f8c3d1635d6e4dd5013bf2348d460d26f",
      "execution": "991cdc2d5c908070d4fb0f67f616aab6dcf4b399f632ff4f1fadbc1119b18e14",
      "readback": "c238c7ee98234e5759cc7e596f2109f75402303be04709bc717dba1d2b16c17f",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-char-to-cp1252-varchar-width1": {
      "metadata": "2a31c3edebe6f9e3bf53592ed4589afd9e411deadf749fd867ddd6c2f38e9e4b",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "0543094bfc735ab0440e3a59e5b56038cf3909a724a3503043e4e9c8d72c014e",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-char-to-cp1252-varchar-width2": {
      "metadata": "2a31c3edebe6f9e3bf53592ed4589afd9e411deadf749fd867ddd6c2f38e9e4b",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "e376eef60343c7647ac2193644bad0f76dec0b6e42eec03ad33490bb0ea53712",
      "readback": "f4651deb3492bc72be9d7983743927cf77ea2703a0168afa93cf0064205a1aaf",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-char-to-cp1252-char-width3": {
      "metadata": "214100c83044b0af77468ec8ec6dcad193c94dd82966948ae65347d6c4345676",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "2ca111eef7e64ebee56f5f1a0d501cf4c57c7e8ca786e151ea02edba5c8f5678",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-char-to-cp1252-char-width4": {
      "metadata": "214100c83044b0af77468ec8ec6dcad193c94dd82966948ae65347d6c4345676",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "3c8b04ceed430bd8b7bbeefb0404aeadbd963e07234d54f8434cae273eb798e0",
      "readback": "73fff24ffda82eb94b14eb1dab159c0434f45ce9a34969eed213f03f90637b18",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-char-to-utf8-varchar-width3": {
      "metadata": "214100c83044b0af77468ec8ec6dcad193c94dd82966948ae65347d6c4345676",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "f9e939a9e30d290b71e05f464cf0acf6e4d0651feea72ea967a6a8f78d84b89c",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-char-to-utf8-varchar-width4": {
      "metadata": "214100c83044b0af77468ec8ec6dcad193c94dd82966948ae65347d6c4345676",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "3c8b04ceed430bd8b7bbeefb0404aeadbd963e07234d54f8434cae273eb798e0",
      "readback": "be8208f04b978e5793d46180d044b783fb864b8e29dad71390e95e27881535b3",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-char-to-utf8-char-width2": {
      "metadata": "bd0baf9ff340ef279d7f87965fecf8f550a385c99b75e0632028d90f1468cb2c",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "d86032a4edfdbe2b01166b8d38933f49a2dece782ade9131766ad3fafb4541b6",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-char-to-utf8-char-width3": {
      "metadata": "bd0baf9ff340ef279d7f87965fecf8f550a385c99b75e0632028d90f1468cb2c",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "62ad139baa8f247d04e2c46a685f23d4d2e4143609be98d9e3c371a61ece993e",
      "readback": "ddfb4be08c90aebeef75c746108279b47b03778695e43cf387423d2f6aa7f431",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-char-to-unicode-nvarchar-width1": {
      "metadata": "bd0baf9ff340ef279d7f87965fecf8f550a385c99b75e0632028d90f1468cb2c",
      "setup": "fbd5b3a34c9f5880de6f8fc1db1814392ca7d32add6b3ad6017aa0e0e73569b5",
      "execution": "62ad139baa8f247d04e2c46a685f23d4d2e4143609be98d9e3c371a61ece993e",
      "readback": "b0858a39c05b6dac21929c4d021b12a4c7a73bc50cf1f6beaf2f2bd7be044915",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-char-to-unicode-nvarchar-width2": {
      "metadata": "bd0baf9ff340ef279d7f87965fecf8f550a385c99b75e0632028d90f1468cb2c",
      "setup": "fbd5b3a34c9f5880de6f8fc1db1814392ca7d32add6b3ad6017aa0e0e73569b5",
      "execution": "62ad139baa8f247d04e2c46a685f23d4d2e4143609be98d9e3c371a61ece993e",
      "readback": "b0858a39c05b6dac21929c4d021b12a4c7a73bc50cf1f6beaf2f2bd7be044915",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-char-to-unicode-nchar-width1": {
      "metadata": "214100c83044b0af77468ec8ec6dcad193c94dd82966948ae65347d6c4345676",
      "setup": "3b96f1db226707756dc755d2ac70581f8c3d1635d6e4dd5013bf2348d460d26f",
      "execution": "fd29b4a3bf3008d174ba9d63f262a71218a7d92d6eae5829ff3dd77a0915a31c",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-char-to-unicode-nchar-width2": {
      "metadata": "214100c83044b0af77468ec8ec6dcad193c94dd82966948ae65347d6c4345676",
      "setup": "3b96f1db226707756dc755d2ac70581f8c3d1635d6e4dd5013bf2348d460d26f",
      "execution": "3c8b04ceed430bd8b7bbeefb0404aeadbd963e07234d54f8434cae273eb798e0",
      "readback": "be8208f04b978e5793d46180d044b783fb864b8e29dad71390e95e27881535b3",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-varchar-declared1-wire2": {
      "metadata": "fb5cba75bac89fb8f3963492f8f00e46560dee9d40539a08d4909e22358cf18e",
      "setup": "589817a5034b8268d52ddeef4ef272d6b5be32f6d85130e9bc860f1f90ad0bf9",
      "execution": "44453e0567f9b847a1a5027417fe549ee4300344f36e05f9970dd14de424877f",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-varchar-declared2-wire1": {
      "metadata": "fb5cba75bac89fb8f3963492f8f00e46560dee9d40539a08d4909e22358cf18e",
      "setup": "589817a5034b8268d52ddeef4ef272d6b5be32f6d85130e9bc860f1f90ad0bf9",
      "execution": "44453e0567f9b847a1a5027417fe549ee4300344f36e05f9970dd14de424877f",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-char-declared1-wire2": {
      "metadata": "474a37df74d037af59ebf3a736b2acfba29b34bdd0129265b25b402f9d0820bd",
      "setup": "589817a5034b8268d52ddeef4ef272d6b5be32f6d85130e9bc860f1f90ad0bf9",
      "execution": "b6d4a0570eb6dc066fa8051a74c47368ec8eeaa7c63d61ad2299270cd222d832",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-char-declared2-wire1": {
      "metadata": "7de6b0ea33d80b595d994baf22fe9ed7521c9c47e2660259ce0d99f3de5d183d",
      "setup": "589817a5034b8268d52ddeef4ef272d6b5be32f6d85130e9bc860f1f90ad0bf9",
      "execution": "b6d4a0570eb6dc066fa8051a74c47368ec8eeaa7c63d61ad2299270cd222d832",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-varchar-declared1-wire2": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "589817a5034b8268d52ddeef4ef272d6b5be32f6d85130e9bc860f1f90ad0bf9",
      "execution": "6e3d90f19f27f8474339f18a268ad71088b9a36fb27a618f1b3a0373d5cff920",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-varchar-declared2-wire1": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "589817a5034b8268d52ddeef4ef272d6b5be32f6d85130e9bc860f1f90ad0bf9",
      "execution": "6e3d90f19f27f8474339f18a268ad71088b9a36fb27a618f1b3a0373d5cff920",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-char-declared1-wire2": {
      "metadata": "2a31c3edebe6f9e3bf53592ed4589afd9e411deadf749fd867ddd6c2f38e9e4b",
      "setup": "589817a5034b8268d52ddeef4ef272d6b5be32f6d85130e9bc860f1f90ad0bf9",
      "execution": "e1b60f95ed0fb953821f80d8775734e10adc45f42939094e15265f921487ff58",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-char-declared2-wire1": {
      "metadata": "f622d3838f490b4d7d08b0173c35f76430fd75645d9129f376a0e54249d3ef39",
      "setup": "589817a5034b8268d52ddeef4ef272d6b5be32f6d85130e9bc860f1f90ad0bf9",
      "execution": "e1b60f95ed0fb953821f80d8775734e10adc45f42939094e15265f921487ff58",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-varchar-declared1-wire2": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "589817a5034b8268d52ddeef4ef272d6b5be32f6d85130e9bc860f1f90ad0bf9",
      "execution": "6e3d90f19f27f8474339f18a268ad71088b9a36fb27a618f1b3a0373d5cff920",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-varchar-declared2-wire1": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "589817a5034b8268d52ddeef4ef272d6b5be32f6d85130e9bc860f1f90ad0bf9",
      "execution": "6e3d90f19f27f8474339f18a268ad71088b9a36fb27a618f1b3a0373d5cff920",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-char-declared1-wire2": {
      "metadata": "2a31c3edebe6f9e3bf53592ed4589afd9e411deadf749fd867ddd6c2f38e9e4b",
      "setup": "589817a5034b8268d52ddeef4ef272d6b5be32f6d85130e9bc860f1f90ad0bf9",
      "execution": "e1b60f95ed0fb953821f80d8775734e10adc45f42939094e15265f921487ff58",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-char-declared2-wire1": {
      "metadata": "f622d3838f490b4d7d08b0173c35f76430fd75645d9129f376a0e54249d3ef39",
      "setup": "589817a5034b8268d52ddeef4ef272d6b5be32f6d85130e9bc860f1f90ad0bf9",
      "execution": "e1b60f95ed0fb953821f80d8775734e10adc45f42939094e15265f921487ff58",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-char-null-empty-to-char4": {
      "metadata": "7de6b0ea33d80b595d994baf22fe9ed7521c9c47e2660259ce0d99f3de5d183d",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "f8894ac220cb2b8b6b9fe7aa6d3dabebba6884dc676f95dc570054ab6249c4ed",
      "readback": "020bd2992d236d2f895f94403caa94d2b1f28f4b9debcba8285c0978a8de5a4c",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-char-null-empty-to-nchar4": {
      "metadata": "7de6b0ea33d80b595d994baf22fe9ed7521c9c47e2660259ce0d99f3de5d183d",
      "setup": "3b96f1db226707756dc755d2ac70581f8c3d1635d6e4dd5013bf2348d460d26f",
      "execution": "f8894ac220cb2b8b6b9fe7aa6d3dabebba6884dc676f95dc570054ab6249c4ed",
      "readback": "175e31034d606c71e875bb787896d4be6cf9b120eefdeac1f528ba0f587399f9",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-char-null-empty-to-char4": {
      "metadata": "f622d3838f490b4d7d08b0173c35f76430fd75645d9129f376a0e54249d3ef39",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "f6957a6c9fb6a9cb800e8a802b7797925544ae03aa13583d9de13adace0876df",
      "readback": "020bd2992d236d2f895f94403caa94d2b1f28f4b9debcba8285c0978a8de5a4c",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-char-null-empty-to-nchar4": {
      "metadata": "f622d3838f490b4d7d08b0173c35f76430fd75645d9129f376a0e54249d3ef39",
      "setup": "3b96f1db226707756dc755d2ac70581f8c3d1635d6e4dd5013bf2348d460d26f",
      "execution": "f6957a6c9fb6a9cb800e8a802b7797925544ae03aa13583d9de13adace0876df",
      "readback": "175e31034d606c71e875bb787896d4be6cf9b120eefdeac1f528ba0f587399f9",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-char-null-empty-to-char4": {
      "metadata": "f622d3838f490b4d7d08b0173c35f76430fd75645d9129f376a0e54249d3ef39",
      "setup": "5d47643dc999477258827c3649c02b3d04ff190e03647b38c915752a143233a7",
      "execution": "f6957a6c9fb6a9cb800e8a802b7797925544ae03aa13583d9de13adace0876df",
      "readback": "020bd2992d236d2f895f94403caa94d2b1f28f4b9debcba8285c0978a8de5a4c",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-char-null-empty-to-nchar4": {
      "metadata": "f622d3838f490b4d7d08b0173c35f76430fd75645d9129f376a0e54249d3ef39",
      "setup": "3b96f1db226707756dc755d2ac70581f8c3d1635d6e4dd5013bf2348d460d26f",
      "execution": "f6957a6c9fb6a9cb800e8a802b7797925544ae03aa13583d9de13adace0876df",
      "readback": "175e31034d606c71e875bb787896d4be6cf9b120eefdeac1f528ba0f587399f9",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1251-max-to-utf8-varchar-max": {
      "metadata": "395d3d2dd0e79a3679023245652a77d2a87af8ea15ff3b2fe9952ccad401a584",
      "setup": "589817a5034b8268d52ddeef4ef272d6b5be32f6d85130e9bc860f1f90ad0bf9",
      "execution": "ad5df50b16b511889090ac5bd61fbde84c007b73b539e93004483a2a2985af5e",
      "readback": "988313e102789549cf847feb6201b30e0fee98d7133b89ce010d87c92694acaf",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "cp1252-max-to-utf8-varchar-max": {
      "metadata": "2ce5635f390844831670aef6bb3b263c2162f6587d2738ee0117a4bacf442ece",
      "setup": "589817a5034b8268d52ddeef4ef272d6b5be32f6d85130e9bc860f1f90ad0bf9",
      "execution": "4e67f13ab77a4fb9de573e1be11f11a8855735448382ef8ec9b1fabd3ffc819f",
      "readback": "988313e102789549cf847feb6201b30e0fee98d7133b89ce010d87c92694acaf",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-max-to-cp1252-varchar-max": {
      "metadata": "2ce5635f390844831670aef6bb3b263c2162f6587d2738ee0117a4bacf442ece",
      "setup": "589817a5034b8268d52ddeef4ef272d6b5be32f6d85130e9bc860f1f90ad0bf9",
      "execution": "0775dc0ab09105b09d59828db261c090af5260c3741e091eadeff54dea3d1e84",
      "readback": "bd4ed0039d1a8747a661a111bc007eb5985c2cd39c39bc8416449a77a0255d8f",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-max-to-unicode-nvarchar-max": {
      "metadata": "2ce5635f390844831670aef6bb3b263c2162f6587d2738ee0117a4bacf442ece",
      "setup": "cb66cedd16bbec9444b1f68a2c74f51060978143c8eb8ea22e6b0007fa23bdd8",
      "execution": "0775dc0ab09105b09d59828db261c090af5260c3741e091eadeff54dea3d1e84",
      "readback": "da574aff7901dac4cc8c7f63f2372814ab81da073318bc9680d7394521f59e2d",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-euro-to-cp1252-varchar-width1": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "97f9f9ae2b95b941a69cc0baf73b7f67419b1f507a7466c6cfb7c1091e02daf7",
      "readback": "2ef0ab53bd632c804a181c7c2d463453ddd942f076a580ac6b6415c513ce2a0b",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    },
    "utf8-euro-to-cp1252-varchar-width3": {
      "metadata": "a436312448ef87b01c40e3c766014c0f17d6bb7bc46d33795b0aaab1f5938ab4",
      "setup": "17b61f6531d7457ee6750ba138f1caf21b894ddd69c8a142d0c47c543f9caaeb",
      "execution": "8cd0d41ee9f59071624ecc9f16aebe0f5cbdc01ffe3f684e60258dbbf244dac2",
      "readback": "ed94624a2a5ff3f09a4a9744b55116b726ec55b81ee486c9864c1a1e6b9882c6",
      "cleanup": "7d526d2bb794dbe0ab44dd6d9bbf8607c39bdb55e136cbec0732d481eeb893b9"
    }
  }
}
const RETAINED_SHA256 = 'cd82a8853bcac9f3fee98c1fb6c6f17564443918868ece1f1d9f66b3abb0ec92'
const REVIEWED_RECAPTURE_SOURCES = []
const targets = [
  ['cp1252','varchar'], ['cp1252','char'], ['utf8','varchar'], ['utf8','char'], ['unicode','nvarchar'], ['unicode','nchar']
]
const common = {declared: 'utf8', wire: 'utf8', target: 'utf8', sourceFamily: 'varchar', sourceWidth: 4, wireWidth: 4, targetFamily: 'varchar', targetWidth: 4}
const entry = (name, options) => ({...common, name, ...options})
export const cases = []
for (const profile of ['cp1251','cp1252','utf8']) for (const sourceFamily of ['varchar','char']) for (const [target,targetFamily] of targets) {
  let hex, widths
  if (profile === 'cp1251') {hex = target === 'utf8' && targetFamily === 'char' ? '41cff042' : 'cff0'; widths = [1,2]}
  else if (profile === 'cp1252') {hex = target === 'utf8' && targetFamily === 'char' ? '41a9e942' : 'a9e9'; widths = [3,4]}
  else if (target === 'cp1252' && targetFamily === 'varchar') {hex = 'cea9'; widths = [1,2]}
  else if (target === 'cp1252' || target === 'utf8' && targetFamily === 'varchar') {hex = 'f09fa686'; widths = [3,4]}
  else if (target === 'utf8') {hex = 'e282ac'; widths = [2,3]}
  else if (targetFamily === 'nvarchar') {hex = 'e282ac'; widths = [1,2]}
  else {hex = 'f09fa686'; widths = [1,2]}
  for (const targetWidth of widths) cases.push(entry(`${profile}-${sourceFamily}-to-${target}-${targetFamily}-width${targetWidth}`, {declared:profile,wire:profile,target,sourceFamily,sourceWidth:hex.length/2,wireWidth:hex.length/2,targetFamily,targetWidth,values:sourceFamily==='char'?[null,hex]:[null,'','41',hex]}))
}
for (const profile of ['cp1251','cp1252','utf8']) for (const sourceFamily of ['varchar','char']) for (const [sourceWidth,wireWidth] of [[1,2],[2,1]]) {
  const hex = {cp1251:'cff0',cp1252:'a9e9',utf8:'cea9'}[profile]
  cases.push(entry(`${profile}-${sourceFamily}-declared${sourceWidth}-wire${wireWidth}`, {declared:profile,wire:profile,sourceFamily,sourceWidth,wireWidth,targetWidth:'max',values:sourceFamily==='char'?[null,hex]:[null,'','41',hex]}))
}
for (const profile of ['cp1251','cp1252','utf8']) for (const [target,targetFamily] of [['utf8','char'],['unicode','nchar']]) cases.push(entry(`${profile}-char-null-empty-to-${targetFamily}4`, {declared:profile,wire:profile,sourceFamily:'char',sourceWidth:1,wireWidth:1,target,targetFamily,values:[null,'','41']}))
for (const [profile,target,targetFamily,hex] of [['cp1251','utf8','varchar','cff0'],['cp1252','utf8','varchar','a9e9'],['utf8','cp1252','varchar','cea9e282acf09fa686'],['utf8','unicode','nvarchar','cea9e282acf09fa686']]) cases.push(entry(`${profile}-max-to-${target}-${targetFamily}-max`, {declared:profile,wire:profile,sourceWidth:'max',wireWidth:'max',target,targetFamily,targetWidth:'max',values:[null,'','41','41'.repeat(401)+hex.repeat(profile==='utf8'?1000:3000)]}))
for (const targetWidth of [1,3]) cases.push(entry(`utf8-euro-to-cp1252-varchar-width${targetWidth}`, {target:'cp1252',sourceWidth:3,wireWidth:3,targetWidth,values:[null,'','41','e282ac']}))
// Equal declared/wire widths with oversized ROW bytes distinguish row-length
// rejection from the separate 4816 declaration/TYPE_INFO mismatch controls.
for (const [replaceName,profile,hex] of [
  ['cp1252-varchar-to-unicode-nvarchar-width4','cp1251','cff0'],
  ['utf8-varchar-to-unicode-nvarchar-width2','utf8','cea9']
]) {
  const index = cases.findIndex(c => c.name === replaceName)
  assert.ok(index >= 0)
  cases[index] = entry(`${profile}-row2-declared1-wire1`, {declared:profile,wire:profile,sourceFamily:'char',sourceWidth:1,wireWidth:1,targetFamily:'char',targetWidth:4,values:[null,'','41',hex]})
}
assert.equal(cases.length, 96)
assert.equal(new Set(cases.map(c => c.name)).size, cases.length)
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
      const load = connection.newBulkLoad('dbo.capacity_probe', {keepNulls: true}, (error, rowCount) => {
        finish({errors, info, rowCount: rowCount ?? null, error: error ? {number: error.number ?? null, code: error.code ?? null, message: error.message} : null})
      })
      load.addColumn('id', TYPES.Int, {nullable: false})
      const type = c.sourceFamily === 'char' ? TYPES.Char : TYPES.VarChar
      const raw = {...type, validate: value => {assert.ok(value === null || Buffer.isBuffer(value)); return value}}
      if (c.wire === 'omitted') raw.generateTypeInfo = (parameter, options) => type.generateTypeInfo(parameter, options).subarray(0, 3)
      load.addColumn('value', raw, {length: c.wireWidth === 'max' ? Infinity : c.wireWidth, nullable: true})
      load.columns[1].collation = collation
      load.getBulkInsertSql = () => `INSERT BULK dbo.capacity_probe ([id] int, [value] ${c.sourceFamily}(${c.sourceWidth}) COLLATE ${profiles[c.declared]}) WITH (KEEP_NULLS)`
      connection.execBulkLoad(load, rows.map(row => ({id: row.id, value: row.valueHex === null ? null : Buffer.from(row.valueHex, 'hex')})))
    } catch (error) {finish({errors, info, rowCount: null, error: {number: error.number ?? null, code: error.code ?? null, message: error.message}})}
  })
}
const readbackSql = `SELECT @@ERROR AS last_error, @@ROWCOUNT AS last_rowcount, XACT_STATE() AS transaction_state, @@TRANCOUNT AS transaction_count;
SELECT id, value, CAST(value AS varbinary(max)) AS native_bytes, CAST(value AS nvarchar(max)) AS unicode_value,
CAST(CAST(value AS nvarchar(max)) AS varbinary(max)) AS unicode_units FROM dbo.capacity_probe ORDER BY id;`
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
      o.setup = await sql(connection, `CREATE TABLE dbo.capacity_probe (id int NOT NULL, value ${targetFamily}(${c.targetWidth}) COLLATE ${targetProfile} NULL)`, retained)
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
      o.cleanup = await sql(connection, 'DROP TABLE dbo.capacity_probe', retained)
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
  assert.equal(o.setup.sql, `CREATE TABLE dbo.capacity_probe (id int NOT NULL, value ${targetFamily}(${c.targetWidth}) COLLATE ${profiles[c.target] ?? profiles.cp1252} NULL)`, 'fixed setup SQL')
  assert.equal(o.readback.sql, readbackSql, 'fixed readback SQL')
  if (o.recoveryReadback) assert.equal(o.recoveryReadback.sql, readbackSql, 'fixed recovery SQL')
  assert.equal(o.cleanup.sql, 'DROP TABLE dbo.capacity_probe', 'fixed cleanup SQL')
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
    if (e.number === 2628) {assert.ok(e.message.startsWith("String or binary data would be truncated in table '" + database + ".dbo.capacity_probe', column 'value'. Truncated value: '")); e.message = e.message.replace(database, '@DATABASE@')}
  }
  if (value.execution.error?.number === 2628) {assert.ok(value.execution.error.message.includes(database + '.dbo.capacity_probe')); value.execution.error.message = value.execution.error.message.replace(database, '@DATABASE@')}
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
  if (args.includes('--replay-fixture')) {validateRetained(await readCaptureFile(fixture)); console.log('Validated retained character capacity capture'); return}
  const output = resolve(positional[0] ?? 'artifacts/compatibility/bulk-character-capacity/capture.json')
  assert.notEqual(output, fixture, 'separate raw output')
  await guardOutput(output); await guardOutput(`${output}.comparison.json`)
  const containers = [], runs = [], retained = budget()
  assert.equal(HELPER_SHA, EXPECTED_HELPER_SHA, 'reviewed helper unchanged before Docker')
  assert.ok(isDeepStrictEqual(HELPERS, EXPECTED_HELPERS), 'reviewed helpers unchanged before Docker')
  const provenance = {clientVersion: CLIENT_VERSION, packetSize: 512, sourceSha: SOURCE_SHA, helperSha: HELPER_SHA, helpers: HELPERS}
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
