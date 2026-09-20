use super::*;
use std::{pin::Pin, task::Poll};

struct Fixture { shared: Shared, reader: Reader, endpoint: UnixStream, _events: mpsc::Receiver<Delivery> }
impl Fixture {
    async fn new() -> Self {
        let (client,endpoint)=UnixStream::pair().unwrap(); let (reader,writer)=client.into_split();
        let (sender,events)=mpsc::channel(64);
        let mut state=State::new(HashSet::new()); state.started=true; state.active=true; state.dimensions=[1,1,8,16];
        let shared=Shared { state:Mutex::new(state),writer:Mutex::new(Box::new(writer)),
            output:Arc::new(Output { sender,capacity:Arc::new(Semaphore::new(OUTPUT_LIMIT)),progress:Arc::new(AtomicU64::new(0)) }),
            pending:StdMutex::new(HashMap::new()),serial:AtomicU64::new(1),changed:Notify::new(),busy:AtomicBool::new(false),progress:AtomicU64::new(0),received:watch::channel(Instant::now()).0 };
        snapshot(&shared,projection(1,"pane")).await.unwrap(); full_surface(&shared,surface(1,"pane")).await.unwrap();
        Self { shared,reader:Box::new(reader),endpoint,_events:events }
    }
    async fn focus(&self)->Focus { let s=self.shared.state.lock().await; Focus { generation:s.generation,pane_id:s.focus.clone().flatten() } }
    fn empty(&self) { assert_eq!(self.endpoint.try_read(&mut [0]).unwrap_err().kind(),io::ErrorKind::WouldBlock); }
}
fn projection(revision:u64,pane:&str)->Value {
    json!({"boot_id":"boot","revision":revision,"focused_pane_id":pane,"focused_tab_id":"tab","focused_workspace_id":"workspace",
        "workspaces":[{"workspace_id":"workspace","label":"1","custom_label":false,"focused":true,"agent_status":"idle"}],
        "tabs":[{"tab_id":"tab","workspace_id":"workspace","label":"1","custom_label":false,"focused":true,"agent_status":"idle"}],
        "panes":[{"pane_id":pane,"workspace_id":"workspace","tab_id":"tab","focused":true}],"agents":[]})
}
fn surface(revision:u64,pane:&str)->Surface {
    Surface { boot:"boot".into(),projection:revision,revision,
        frame:codec::Frame { cells:vec![codec::Cell { symbol:" ".into(),fg:0,bg:0,modifier:0,skip:false,hyperlink:None }],width:1,height:1,cursor:None,links:vec![],graphics:vec![] },
        panes:vec![codec::Pane { id:pane.into(),rect:[0,0,1,1],inner:[0,0,1,1],focused:true }],popup:None,
        scene:codec::Scene { assets:vec![],placements:vec![],retained:vec![] } }
}
async fn pending<T>(mut future:Pin<&mut impl Future<Output=T>>) { std::future::poll_fn(|cx| { assert!(future.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await }
fn expected()->Vec<u8> { let mut bytes=vec![13];codec::string(&mut bytes,"pane");bytes.extend([1,1]);codec::string(&mut bytes,"x");bytes }

#[tokio::test]
async fn same_pane_snapshot_waits_for_matching_surface_and_sends_once() {
    let mut f=Fixture::new().await; let focus=f.focus().await;
    snapshot(&f.shared,projection(2,"pane")).await.unwrap();
    { let event=Input::Text("x".into()); let input=input_event(&f.shared,&focus,&event);tokio::pin!(input);
      pending(input.as_mut()).await;f.empty();
      full_surface(&f.shared,surface(2,"pane")).await.unwrap();input.await.unwrap(); }
    assert_eq!(packet(&mut f.endpoint).await.unwrap(),expected());f.empty();
    assert_eq!(f.focus().await,focus);
}
#[tokio::test]
async fn changed_pane_during_surface_wait_never_retargets() {
    let f=Fixture::new().await;let focus=f.focus().await;
    snapshot(&f.shared,projection(2,"pane")).await.unwrap();
    let event=Input::Text("x".into());let input=input_event(&f.shared,&focus,&event);tokio::pin!(input);pending(input.as_mut()).await;
    snapshot(&f.shared,projection(3,"other")).await.unwrap();full_surface(&f.shared,surface(3,"other")).await.unwrap();
    assert_eq!(input.await.unwrap_err().code,"stale_focus");f.empty();
}
#[tokio::test]
async fn popup_transition_during_surface_wait_rejects_old_input() {
    let f=Fixture::new().await;let focus=f.focus().await;snapshot(&f.shared,projection(2,"pane")).await.unwrap();
    let event=Input::Text("x".into());let input=input_event(&f.shared,&focus,&event);tokio::pin!(input);pending(input.as_mut()).await;
    let mut view=surface(2,"pane");view.popup=Some(codec::Popup { id:"popup".into(),frame:surface(2,"pane").frame });full_surface(&f.shared,view).await.unwrap();
    assert_eq!(input.await.unwrap_err().code,"stale_focus");f.empty();
}
#[tokio::test]
async fn cancellation_during_surface_wait_sends_nothing_later() {
    let f=Fixture::new().await;let focus=f.focus().await;snapshot(&f.shared,projection(2,"pane")).await.unwrap();
    { let event=Input::Text("x".into());let input=input_event(&f.shared,&focus,&event);tokio::pin!(input);pending(input.as_mut()).await; }
    full_surface(&f.shared,surface(2,"pane")).await.unwrap();f.empty();
}
#[tokio::test]
async fn changed_boot_rejected_without_reusing_pane_identity() {
    let f=Fixture::new().await;let mut next=projection(2,"pane");next["boot_id"]="replacement".into();
    assert_eq!(snapshot(&f.shared,next).await.unwrap_err().code,"stale_boot");f.empty();
}
#[tokio::test]
async fn stalled_surface_times_out_without_sending() {
    let f=Fixture::new().await;let focus=f.focus().await;snapshot(&f.shared,projection(2,"pane")).await.unwrap();
    assert_eq!(input_event(&f.shared,&focus,&Input::Text("x".into())).await.unwrap_err().code,"timeout");f.empty();
}

#[tokio::test]
async fn active_resize_sends_only_stock_resize_without_reactivation() {
    let Fixture { shared, reader:_, mut endpoint, _events }=Fixture::new().await;
    let (sender,receiver)=mpsc::channel(1);
    let worker=tokio::spawn(async move { commands(&shared,receiver).await });
    sender.send(Command::Resize([82,46,15,31])).await.unwrap();
    let packet=packet(&mut endpoint).await.unwrap();
    let mut decoder=Decoder::new(&packet);
    assert_eq!(decoder.u32().unwrap(),12);
    assert_eq!([decoder.uint().unwrap(),decoder.uint().unwrap(),decoder.uint().unwrap(),decoder.uint().unwrap()],[15,31,82,46]);
    assert!(!decoder.boolean().unwrap()); decoder.finish().unwrap();
    assert_eq!(endpoint.try_read(&mut [0]).unwrap_err().kind(),io::ErrorKind::WouldBlock);
    drop(sender); worker.await.unwrap().unwrap();
}

#[tokio::test]
async fn click_targets_current_surface_pane_with_down_and_up() {
    let mut fixture=Fixture::new().await;let focus=fixture.focus().await;
    input_event(&fixture.shared,&focus,&Input::Click{pane:"pane".into(),column:0,row:0,right:false}).await.unwrap();
    let mut expected=vec![13];codec::string(&mut expected,"pane");expected.push(2);
    expected.extend([2,0,0,0,0,0,0,0,1,2,1,0,0,0,0,0,0,1]);
    assert_eq!(packet(&mut fixture.endpoint).await.unwrap(),expected);
}

#[tokio::test]
async fn ordinary_endpoint_traffic_satisfies_health_but_silence_expires() {
    let Fixture { shared, mut reader, endpoint: mut peer, _events } = Fixture::new().await;
    let client = async {
        tokio::select! {
            result = endpoint(&shared, &mut reader) => result,
            result = health(&shared) => result,
        }
    };
    let server = async {
        let first = packet(&mut peer).await.unwrap();
        let mut d = Decoder::new(&first);
        assert_eq!(d.u32().unwrap(), 20);
        assert_eq!(d.string().unwrap(), "endpoint.health.ping.v1");
        d.string().unwrap(); d.finish().unwrap();
        // A real non-pong control packet must keep the connection alive.
        let traffic = codec::control("future.optional.control", "alive");
        peer.write_all(&(traffic.len() as u32).to_le_bytes()).await.unwrap();
        peer.write_all(&traffic).await.unwrap();
        let next = timeout(TIMEOUT, packet(&mut peer)).await.unwrap().unwrap();
        let mut d = Decoder::new(&next);
        assert_eq!(d.u32().unwrap(), 20);
        assert_eq!(d.string().unwrap(), "endpoint.health.ping.v1");
        let sent = Instant::now();
        // Keep the transport open but silent, rather than testing EOF.
        tokio::time::sleep(TIMEOUT + Duration::from_secs(1)).await;
        sent
    };
    let (result, last_probe) = tokio::join!(client, server);
    assert_eq!(result.unwrap_err().code, "timeout");
    assert!(last_probe.elapsed() >= TIMEOUT);
}
