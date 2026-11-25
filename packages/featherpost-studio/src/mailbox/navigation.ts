import {m} from '@featherpost/i18n/messages';
import type {OpenIntent} from '../bridge/types';
import type {MailboxController} from './controller';
/** The guard starts before saving, so a later user navigation wins throughout the handoff. */
export async function followMailIntent(controller:MailboxController,intent:OpenIntent,save:()=>Promise<void>,open:()=>void|boolean,notify:(text:string)=>void,approve?:()=>Promise<boolean>){
 const current=controller.navigationGuard();
 try{await save();}catch{if(current())notify(m.mailbox_navigation_unsaved_draft());return;}
 if(!current())return;
 if(approve&&!(await approve()))return;
 if(!current())return;
 if(open()===false)return;
 if(intent.route==='message'&&'account' in intent&&'uid' in intent&&typeof intent.account==='string'&&typeof intent.uid==='string')await controller.locate(intent.account,intent.uid,'folder' in intent&&typeof intent.folder==='string'?intent.folder:undefined);
 else if('account' in intent&&typeof intent.account==='string')controller.selectAccount(intent.account);
 else controller.selectSmart('inbox');
}
